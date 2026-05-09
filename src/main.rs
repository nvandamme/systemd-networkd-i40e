// ----------------------------------------------------------------
// i40e/X710 post-link helper (PF-level) for SR-IOV + VM/LXC passthrough
// ----------------------------------------------------------------
// - Invoked from systemd-networkd .link via systemd unit (ExecStart)
// - Arg: PF interface name (e.g. enp2s0f0np0)
// - Reads custom I40E_* props from .link via udevadm info
// - Applies ALL ethtool tunables first (PF -> VF), then ALL ip(8) tunables
// - Derives contiguous VF MACs from PF MAC (LAA+unicast safe), unless admin MAC exists
// - VLAN_QUIRK (VM quirk): enforce rxvlan off (PF+VF) then bounce VF VLAN 10 -> 0
// - ASYM_QUIRK: disable PF TX checksumming (ip-generic,sctp) + symmetric coalesce
// - Default useful priv-flags: disable-fw-lldp on; vf-true-promisc-support if supported
// - Logs INFO on success, WARNING on failure via stderr with syslog-style priorities
// ----------------------------------------------------------------
// References:
// - VLAN tag stripping: https://community.intel.com/t5/Ethernet-Products/X710-strips-incoming-vlan-tag-with-SRIOV/m-p/551464
// - Host-path asymmetry: https://community.intel.com/t5/Ethernet-Products/Intel-X710-SFP-Asymmetric-Performance-Issue-Upload-Download/m-p/1685603
// ----------------------------------------------------------------

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CliToggles {
    dry_run: Option<bool>,
    derive_vf_macs: Option<bool>,
    skip_if_vf_mac_set: Option<bool>,
    enable_vf_true_promisc: Option<bool>,
    disable_fw_lldp: Option<bool>,
    asym_quirk: Option<bool>,
    vlan_quirk: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliOptions {
    ifname: String,
    toggles: CliToggles,
}

#[derive(Debug, PartialEq, Eq)]
enum ParseOutcome {
    Run(CliOptions),
    Help(String),
}

#[derive(Debug)]
struct App {
    ifname: String,
    i40e_env: BTreeMap<String, String>,
    sys_class_net_root: PathBuf,
    cli_toggles: CliToggles,
    dry_run: bool,
    derive_vf_macs: bool,
    skip_if_vf_mac_set: bool,
    enable_vf_true_promisc: bool,
    disable_fw_lldp: bool,
    asym_quirk: bool,
    vlan_quirk: bool,
}

#[derive(Debug, Default)]
struct VfSettings {
    dev: Option<String>,
    mac: Option<String>,
    q_vlan: Option<String>,
    q_asym: Option<String>,
}

fn main() -> ExitCode {
    let program = env::args()
        .next()
        .unwrap_or_else(|| "i40e-postlink".to_string());

    match parse_args(env::args().skip(1), &program) {
        Ok(ParseOutcome::Run(options)) => {
            let mut app = App::new(options);
            app.run();
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Help(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}\n\n{}", usage(&program));
            ExitCode::from(2)
        }
    }
}

impl App {
    fn new(options: CliOptions) -> Self {
        let CliOptions { ifname, toggles } = options;
        Self {
            ifname,
            i40e_env: env::vars()
                .filter(|(key, _)| key.starts_with("I40E_"))
                .collect(),
            sys_class_net_root: env::var("I40E_SYS_CLASS_NET_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/sys/class/net")),
            cli_toggles: toggles.clone(),
            dry_run: toggles.dry_run.unwrap_or(env_toggle("DRY_RUN", false)),
            derive_vf_macs: env_toggle("DERIVE_VF_MACS", true),
            skip_if_vf_mac_set: env_toggle("SKIP_IF_VF_MAC_SET", true),
            enable_vf_true_promisc: env_toggle("ENABLE_VF_TRUE_PROMISC", true),
            disable_fw_lldp: env_toggle("DISABLE_FW_LLDP", true),
            asym_quirk: env_toggle("ASYM_QUIRK", true),
            vlan_quirk: env_toggle("VLAN_QUIRK", true),
        }
    }

    fn run(&mut self) {
        // ---------- read I40E_* from .link ----------
        self.load_link_props();

        if self.dry_run {
            log_info("DRY_RUN: Listing preset I40E_* environment variables:");
            for (key, value) in &self.i40e_env {
                log_info(&format!("DRY_RUN: {key}={value}"));
            }
        }

        // ---------- PF ethtool phase ----------
        self.apply_from_props_pf();
        self.apply_cli_overrides();

        let priv_flags = self.show_priv_flags();
        if priv_flags.contains("disable-fw-lldp") {
            self.run_apply(
                "ethtool",
                &[
                    "--set-priv-flags".into(),
                    self.ifname.clone(),
                    "disable-fw-lldp".into(),
                    if self.disable_fw_lldp { "on" } else { "off" }.into(),
                ],
            );
        } else {
            log_warn(&format!(
                "priv-flag disable-fw-lldp not supported on {}; skipping",
                self.ifname
            ));
        }

        if self.enable_vf_true_promisc {
            if priv_flags.contains("vf-true-promisc-support") {
                self.run_apply(
                    "ethtool",
                    &[
                        "--set-priv-flags".into(),
                        self.ifname.clone(),
                        "vf-true-promisc-support".into(),
                        "on".into(),
                    ],
                );
            } else {
                log_warn(&format!(
                    "priv-flag vf-true-promisc-support not supported on {}; skipping",
                    self.ifname
                ));
            }
        }

        // ASYMMETRY QUIRK (PF host path)
        if self.asym_quirk {
            self.apply_ethtool_tunables(
                &self.ifname,
                "C",
                "adaptive-rx off adaptive-tx off rx-usecs 0 tx-usecs 0",
            );
            self.apply_ethtool_tunables(
                &self.ifname,
                "K",
                "tx-checksum-ip-generic off tx-checksum-sctp off",
            );
        }

        // ---------- derive VF MAC prefix ----------
        let mac_prefix = if self.derive_vf_macs {
            let prefix = self.derive_mac_prefix();
            if let Some(value) = prefix.as_deref() {
                log_info(&format!(
                    "derived VF MAC prefix from {}: {value}xx",
                    self.ifname
                ));
            }
            prefix
        } else {
            None
        };

        // ---------- Per-VF loop (ethtool -> ip) ----------
        for idx in self.list_vf_indices() {
            let mut vf_settings = self.apply_from_props_vf(idx);

            // VLAN quirk: ensure rxvlan off before IP ops.
            if self.vlan_quirk || vf_settings.q_vlan.is_some() {
                self.ensure_rxvlan_off(&self.ifname);
                if vf_settings.dev.is_none() {
                    vf_settings.dev = self.resolve_vf_dev(idx);
                }
                if let Some(dev) = vf_settings.dev.as_deref() {
                    self.ensure_rxvlan_off(dev);
                }
            }

            // IP-level baseline for VFs.
            self.apply_ip_vf_tokens(idx, vec!["spoofchk".into(), "off".into()]);
            self.apply_ip_vf_tokens(idx, vec!["trust".into(), "on".into()]);

            // Explicit admin MAC.
            if let Some(mac) = vf_settings.mac.as_deref().filter(|value| is_hex_mac(value)) {
                if let Some(set_mac) = sanitize_llaa_unicast(mac) {
                    self.apply_ip_vf_tokens(idx, vec!["mac".into(), set_mac.clone()]);
                    log_info(&format!("VF {idx}: explicit admin MAC {set_mac}"));
                }
            }

            // Derived admin MAC (if allowed and not already set).
            if self.derive_vf_macs {
                if let Some(prefix) = mac_prefix.as_deref() {
                    let mut do_set = true;
                    if self.skip_if_vf_mac_set {
                        if let Some(current) =
                            self.get_vf_admin_mac(idx).filter(|value| is_hex_mac(value))
                        {
                            do_set = false;
                            log_info(&format!("VF {idx}: keep existing admin MAC ({current})"));
                        }
                    }

                    if do_set {
                        let last = format!("{idx:02x}");
                        if let Some(new_mac) = sanitize_llaa_unicast(&format!("{prefix}{last}")) {
                            self.apply_ip_vf_tokens(idx, vec!["mac".into(), new_mac.clone()]);
                            log_info(&format!("VF {idx}: set derived MAC {new_mac}"));
                        }
                    }
                }
            }

            // VLAN bounce if requested.
            if self.vlan_quirk || vf_settings.q_vlan.is_some() {
                self.apply_ip_vf_tokens(idx, vec!["vlan".into(), "10".into()]);
                self.apply_ip_vf_tokens(idx, vec!["vlan".into(), "0".into()]);
            }
        }

        log_info(&format!("postlink completed on {}", self.ifname));
    }

    // ---------- read I40E_* from .link ----------
    fn load_link_props(&mut self) {
        let sys_net_path = self.net_path(&self.ifname);
        let props = self.capture_success_output(
            "udevadm",
            vec![
                "info".into(),
                "-q".into(),
                "property".into(),
                "-p".into(),
                sys_net_path.display().to_string(),
            ],
        );

        let Some(props) = props else {
            return;
        };

        for line in props.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            if !key.starts_with("I40E_") || self.i40e_env.contains_key(key) {
                continue;
            }

            self.i40e_env.insert(
                key.to_string(),
                strip_wrapping_quotes(value.trim()).to_string(),
            );
        }
    }

    // ---------- apply from PF properties ----------
    fn apply_from_props_pf(&mut self) {
        if let Some(priv_flags) = self.prop_owned("I40E_PRIVFLAGS") {
            let tokens = split_tokens(&priv_flags);
            let mut index = 0;
            while index + 1 < tokens.len() {
                self.run_apply(
                    "ethtool",
                    &[
                        "--set-priv-flags".into(),
                        self.ifname.clone(),
                        tokens[index].clone(),
                        tokens[index + 1].clone(),
                    ],
                );
                index += 2;
            }
        }

        for (key, family) in [
            ("I40E_PF_OFFLOAD", "K"),
            ("I40E_PF_COALESCE", "C"),
            ("I40E_PF_PAUSE", "A"),
            ("I40E_PF_RINGS", "G"),
            ("I40E_PF_CHANNELS", "L"),
            ("I40E_PF_RSS", "rss"),
            ("I40E_PF_RXFH", "rxfh"),
            ("I40E_PF_NTUPLE", "N"),
        ] {
            if let Some(value) = self.prop_owned(key) {
                self.apply_ethtool_tunables(&self.ifname, family, &value);
            }
        }

        for (key, subcommand) in [
            ("I40E_PF_TXQUEUELEN", "txqueuelen"),
            ("I40E_PF_ALIAS", "alias"),
            ("I40E_PF_ARP", "arp"),
            ("I40E_PF_MULTICAST", "multicast"),
            ("I40E_PF_ALLMULTICAST", "allmulticast"),
            ("I40E_PF_PROMISC", "promisc"),
            ("I40E_PF_MAC", "address"),
            ("I40E_PF_MTU", "mtu"),
        ] {
            if let Some(value) = self.prop_owned(key) {
                self.apply_ip_dev_tokens(&self.ifname, vec![subcommand.into(), value]);
            }
        }

        if let Some(value) = self.prop("I40E_ASYM_QUIRK") {
            self.asym_quirk = value == "1";
        }
        if let Some(value) = self.prop("I40E_VLAN_QUIRK") {
            self.vlan_quirk = value == "1";
        }
        if let Some(value) = self.prop("I40E_DERIVE_VF_MACS") {
            self.derive_vf_macs = value == "1";
        }
        if let Some(value) = self.prop("I40E_SKIP_IF_VF_MAC_SET") {
            self.skip_if_vf_mac_set = value == "1";
        }
        if let Some(value) = self.prop("I40E_ENABLE_VF_TRUE_PROMISC") {
            self.enable_vf_true_promisc = value == "1";
        }
        if let Some(value) = self.prop("I40E_DISABLE_FW_LLDP") {
            self.disable_fw_lldp = value == "1";
        }
    }

    fn apply_cli_overrides(&mut self) {
        if let Some(value) = self.cli_toggles.dry_run {
            self.dry_run = value;
        }
        if let Some(value) = self.cli_toggles.derive_vf_macs {
            self.derive_vf_macs = value;
        }
        if let Some(value) = self.cli_toggles.skip_if_vf_mac_set {
            self.skip_if_vf_mac_set = value;
        }
        if let Some(value) = self.cli_toggles.enable_vf_true_promisc {
            self.enable_vf_true_promisc = value;
        }
        if let Some(value) = self.cli_toggles.disable_fw_lldp {
            self.disable_fw_lldp = value;
        }
        if let Some(value) = self.cli_toggles.asym_quirk {
            self.asym_quirk = value;
        }
        if let Some(value) = self.cli_toggles.vlan_quirk {
            self.vlan_quirk = value;
        }
    }

    // ---------- apply from VF properties (explicit quirks only) ----------
    fn apply_from_props_vf(&self, idx: usize) -> VfSettings {
        let vf_settings = VfSettings {
            dev: self.resolve_vf_dev(idx),
            mac: self.vf_specific(idx, "MAC"),
            q_vlan: self.vf_specific(idx, "VLAN_QUIRK"),
            q_asym: self.vf_specific(idx, "ASYM_QUIRK"),
        };

        let vf_offload = self.vf_with_global(idx, "OFFLOAD");
        let vf_coalesce = self.vf_with_global(idx, "COALESCE");
        let vf_pause = self.vf_with_global(idx, "PAUSE");
        let vf_rings = self.vf_with_global(idx, "RINGS");
        let vf_channels = self.vf_with_global(idx, "CHANNELS");
        let mut vf_rss = self.vf_with_global(idx, "RSS");
        if vf_rss.is_none() {
            vf_rss = self
                .vf_specific(idx, "RXFH")
                .or_else(|| self.vf_global("RXFH"));
        }
        let vf_ntuple = self.vf_with_global(idx, "NTUPLE");

        if let Some(dev) = vf_settings.dev.as_deref() {
            if let Some(value) = vf_offload.as_deref() {
                self.apply_ethtool_tunables(dev, "K", value);
            }
            if let Some(value) = vf_coalesce.as_deref() {
                self.apply_ethtool_tunables(dev, "C", value);
            }
            if let Some(value) = vf_pause.as_deref() {
                self.apply_ethtool_tunables(dev, "A", value);
            }
            if let Some(value) = vf_rings.as_deref() {
                self.apply_ethtool_tunables(dev, "G", value);
            }
            if let Some(value) = vf_channels.as_deref() {
                self.apply_ethtool_tunables(dev, "L", value);
            }
            if let Some(value) = vf_rss.as_deref() {
                self.apply_ethtool_tunables(dev, "rss", value);
            }
            if let Some(value) = vf_ntuple.as_deref() {
                self.apply_ethtool_tunables(dev, "N", value);
            }
        } else {
            log_warn(&format!(
                "VF {idx}: no host-side netdev found; skipping VF-netdev ethtool/ip tunables"
            ));
        }

        if let Some(value) = self.vf_specific(idx, "VLAN") {
            self.apply_ip_vf_tokens(idx, vec!["vlan".into(), value]);
        }
        if let Some(value) = self.vf_with_global(idx, "RATE") {
            self.apply_ip_vf_tokens(idx, vec!["rate".into(), value]);
        }
        if let Some(value) = self.vf_with_global(idx, "SPOOFCHK") {
            self.apply_ip_vf_tokens(idx, vec!["spoofchk".into(), value]);
        }
        if let Some(value) = self.vf_with_global(idx, "STATE") {
            self.apply_ip_vf_tokens(idx, vec!["state".into(), value]);
        }

        let vf_query_rss = self.vf_with_global(idx, "QUERY_RSS");
        if let Some(value) = vf_query_rss.as_deref() {
            self.apply_ip_vf_tokens(idx, vec!["query_rss".into(), normalize_on_off(value)]);
        }
        if let Some(value) = self.vf_with_global(idx, "TRUST") {
            self.apply_ip_vf_tokens(idx, vec!["trust".into(), value]);
        }
        if let Some(value) = self.vf_with_global(idx, "MAX_TX_RATE") {
            self.apply_ip_vf_tokens(idx, vec!["max_tx_rate".into(), value]);
        }
        if let Some(value) = self.vf_with_global(idx, "MIN_TX_RATE") {
            self.apply_ip_vf_tokens(idx, vec!["min_tx_rate".into(), value]);
        }
        if vf_query_rss.is_none() && vf_rss.is_some() {
            self.apply_ip_vf_tokens(idx, vec!["query_rss".into(), "on".into()]);
        }

        let vf_ip_txqueuelen = self.vf_with_global(idx, "TXQUEUELEN");
        let vf_ip_mtu = self.vf_with_global(idx, "MTU");
        let vf_ip_alias = self.vf_specific(idx, "ALIAS");
        let vf_ip_arp = self.vf_with_global(idx, "ARP");
        let vf_ip_multicast = self.vf_with_global(idx, "MULTICAST");
        let vf_ip_allmulticast = self.vf_with_global(idx, "ALLMULTICAST");
        let vf_ip_promisc = self.vf_with_global(idx, "PROMISC");

        if let Some(dev) = vf_settings.dev.as_deref() {
            if let Some(value) = vf_ip_txqueuelen {
                self.apply_ip_dev_tokens(dev, vec!["txqueuelen".into(), value]);
            }
            if let Some(value) = vf_ip_mtu {
                self.apply_ip_dev_tokens(dev, vec!["mtu".into(), value]);
            }
            if let Some(value) = vf_ip_alias {
                self.apply_ip_dev_tokens(dev, vec!["alias".into(), value]);
            }
            if let Some(value) = vf_ip_arp {
                self.apply_ip_dev_tokens(dev, vec!["arp".into(), value]);
            }
            if let Some(value) = vf_ip_multicast {
                self.apply_ip_dev_tokens(dev, vec!["multicast".into(), value]);
            }
            if let Some(value) = vf_ip_allmulticast {
                self.apply_ip_dev_tokens(dev, vec!["allmulticast".into(), value]);
            }
            if let Some(value) = vf_ip_promisc {
                self.apply_ip_dev_tokens(dev, vec!["promisc".into(), value]);
            }
        }

        if vf_settings
            .q_asym
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            log_info(&format!(
                "VF {idx}: ASYM_QUIRK requested (host-path quirk); no VF-side action"
            ));
        }

        vf_settings
    }

    // ---------- generic applicators ----------
    // apply_ethtool_tunables <dev> <FAM|--long|-K|rss> [args...]
    fn apply_ethtool_tunables(&self, dev: &str, family: &str, raw_args: &str) {
        let Some(short) = ethtool_family_short(family) else {
            log_warn(&format!("unknown ethtool family '{family}' for {dev}"));
            return;
        };

        let mut tokens = split_tokens(raw_args);
        if tokens
            .first()
            .is_some_and(|token| is_ethtool_family_token(token))
        {
            tokens.remove(0);
        }
        if tokens.is_empty() {
            return;
        }

        let mut args = vec![short.into(), dev.to_string()];
        args.extend(tokens);
        self.run_apply("ethtool", &args);
    }

    fn apply_ip_dev_tokens(&self, dev: &str, tokens: Vec<String>) {
        if tokens.is_empty() {
            return;
        }

        let mut args = vec!["link".into(), "set".into(), "dev".into(), dev.to_string()];
        args.extend(tokens);
        self.run_apply("ip", &args);
    }

    fn apply_ip_vf_tokens(&self, idx: usize, tokens: Vec<String>) {
        if tokens.is_empty() {
            return;
        }

        let mut args = vec![
            "link".into(),
            "set".into(),
            "dev".into(),
            self.ifname.clone(),
            "vf".into(),
            idx.to_string(),
        ];
        args.extend(tokens);
        self.run_apply("ip", &args);
    }

    fn ensure_rxvlan_off(&self, dev: &str) {
        self.apply_ethtool_tunables(dev, "K", "rxvlan off");
    }

    // Mutating runner: always log, never abort.
    fn run_apply(&self, program: &str, args: &[String]) {
        let rendered = render_command(program, args);
        if self.dry_run {
            // Do not execute; log exactly what would run.
            log_info(&format!("DRY: {rendered}"));
            return;
        }

        let Some((success, output, rc)) = run_command(program, args) else {
            log_warn(&format!(
                "cmd failed (rc=1): {rendered} :: failed to spawn command"
            ));
            return;
        };

        if success {
            log_info(&format!("applied: {rendered}"));
        } else {
            log_warn(&format!(
                "cmd failed (rc={rc}): {rendered} :: {}",
                output.trim()
            ));
        }
    }

    fn capture_success_output(&self, program: &str, args: Vec<String>) -> Option<String> {
        let (success, output, _) = run_command(program, &args)?;
        success.then_some(output)
    }

    fn show_priv_flags(&self) -> String {
        self.capture_success_output(
            "ethtool",
            vec!["--show-priv-flags".into(), self.ifname.clone()],
        )
        .unwrap_or_default()
    }

    fn derive_mac_prefix(&self) -> Option<String> {
        let pf_mac = fs::read_to_string(self.net_path(&self.ifname).join("address"))
            .ok()?
            .trim()
            .to_string();
        derive_mac_prefix_from(&pf_mac, self.pf_func_id())
    }

    fn pf_func_id(&self) -> u8 {
        let path = fs::canonicalize(self.net_path(&self.ifname).join("device")).ok();
        let Some(path) = path else {
            return 0;
        };

        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            return 0;
        };

        let raw = name.rsplit('.').next().unwrap_or("0");
        u8::from_str_radix(raw, 16).unwrap_or(0)
    }

    fn list_vf_indices(&self) -> Vec<usize> {
        let device_path = self.net_path(&self.ifname).join("device");

        let Ok(entries) = fs::read_dir(device_path) else {
            return Vec::new();
        };

        let mut indices = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("virtfn") {
                continue;
            }

            let digits: String = name
                .chars()
                .filter(|value| value.is_ascii_digit())
                .collect();
            if let Ok(index) = digits.parse::<usize>() {
                indices.push(index);
            }
        }

        indices.sort_unstable();
        indices.dedup();
        indices
    }

    fn resolve_vf_dev(&self, idx: usize) -> Option<String> {
        self.vf_if_candidates(idx)
            .into_iter()
            .find(|candidate| self.net_path(candidate).exists())
    }

    fn vf_if_candidates(&self, idx: usize) -> Vec<String> {
        let primary = format!("{}v{idx}", self.ifname);
        let trimmed = trim_np_suffix(&self.ifname);
        let secondary = format!("{trimmed}v{idx}");
        if primary == secondary {
            vec![primary]
        } else {
            vec![primary, secondary]
        }
    }

    fn get_vf_admin_mac(&self, idx: usize) -> Option<String> {
        let output = self.capture_success_output(
            "ip",
            vec![
                "-d".into(),
                "link".into(),
                "show".into(),
                self.ifname.clone(),
            ],
        )?;
        parse_vf_admin_mac(&output, idx)
    }

    fn prop(&self, key: &str) -> Option<&str> {
        self.i40e_env.get(key).map(String::as_str)
    }

    fn prop_owned(&self, key: &str) -> Option<String> {
        self.i40e_env.get(key).cloned()
    }

    fn vf_specific(&self, idx: usize, suffix: &str) -> Option<String> {
        self.prop_owned(&format!("I40E_VF{idx}_{suffix}"))
    }

    fn vf_global(&self, suffix: &str) -> Option<String> {
        self.prop_owned(&format!("I40E_VF_{suffix}"))
    }

    fn vf_with_global(&self, idx: usize, suffix: &str) -> Option<String> {
        self.vf_specific(idx, suffix)
            .or_else(|| self.vf_global(suffix))
    }

    fn net_path(&self, ifname: &str) -> PathBuf {
        self.sys_class_net_root.join(ifname)
    }
}

fn env_toggle(name: &str, default_value: bool) -> bool {
    env::var(name).map_or(default_value, |value| value == "1")
}

// ---------- argv / defaults ----------
fn parse_args<I>(args: I, program: &str) -> Result<ParseOutcome, String>
where
    I: IntoIterator<Item = String>,
{
    let mut toggles = CliToggles::default();
    let mut ifname: Option<String> = None;
    let mut args = args.into_iter();
    let mut positional_only = false;

    while let Some(arg) = args.next() {
        if !positional_only {
            match arg.as_str() {
                "-h" | "--help" => return Ok(ParseOutcome::Help(usage(program))),
                "--" => {
                    positional_only = true;
                    continue;
                }
                "--interface" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "missing value for --interface".to_string())?;
                    set_ifname(&mut ifname, value)?;
                    continue;
                }
                "--dry-run" => {
                    toggles.dry_run = Some(true);
                    continue;
                }
                "--no-dry-run" => {
                    toggles.dry_run = Some(false);
                    continue;
                }
                "--derive-vf-macs" => {
                    toggles.derive_vf_macs = Some(true);
                    continue;
                }
                "--no-derive-vf-macs" => {
                    toggles.derive_vf_macs = Some(false);
                    continue;
                }
                "--skip-if-vf-mac-set" => {
                    toggles.skip_if_vf_mac_set = Some(true);
                    continue;
                }
                "--no-skip-if-vf-mac-set" => {
                    toggles.skip_if_vf_mac_set = Some(false);
                    continue;
                }
                "--enable-vf-true-promisc" => {
                    toggles.enable_vf_true_promisc = Some(true);
                    continue;
                }
                "--no-enable-vf-true-promisc" => {
                    toggles.enable_vf_true_promisc = Some(false);
                    continue;
                }
                "--disable-fw-lldp" => {
                    toggles.disable_fw_lldp = Some(true);
                    continue;
                }
                "--no-disable-fw-lldp" => {
                    toggles.disable_fw_lldp = Some(false);
                    continue;
                }
                "--asym-quirk" => {
                    toggles.asym_quirk = Some(true);
                    continue;
                }
                "--no-asym-quirk" => {
                    toggles.asym_quirk = Some(false);
                    continue;
                }
                "--vlan-quirk" => {
                    toggles.vlan_quirk = Some(true);
                    continue;
                }
                "--no-vlan-quirk" => {
                    toggles.vlan_quirk = Some(false);
                    continue;
                }
                _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
                _ => {}
            }
        }

        set_ifname(&mut ifname, arg)?;
    }

    let ifname = ifname.ok_or_else(|| "missing PF interface name".to_string())?;
    Ok(ParseOutcome::Run(CliOptions { ifname, toggles }))
}

fn set_ifname(slot: &mut Option<String>, value: String) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("unexpected argument: {value}"));
    }
    *slot = Some(value);
    Ok(())
}

fn usage(program: &str) -> String {
    format!(
        concat!(
            "usage: {program} [options] <pf-ifname>\n",
            "\n",
            "Options:\n",
            "  --interface <pf-ifname>           PF interface name as an explicit switch\n",
            "  --dry-run | --no-dry-run         Enable or disable dry-run mode\n",
            "  --derive-vf-macs | --no-derive-vf-macs\n",
            "  --skip-if-vf-mac-set | --no-skip-if-vf-mac-set\n",
            "  --enable-vf-true-promisc | --no-enable-vf-true-promisc\n",
            "  --disable-fw-lldp | --no-disable-fw-lldp\n",
            "  --asym-quirk | --no-asym-quirk\n",
            "  --vlan-quirk | --no-vlan-quirk\n",
            "  -h, --help                       Show this help text\n",
            "\n",
            "Precedence: CLI switches > environment variables > ",
            ".link Property=I40E_* > built-in defaults.",
        ),
        program = program,
    )
}

// ---------- helpers ----------
fn strip_wrapping_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|trimmed| trimmed.strip_suffix('"'))
        .unwrap_or(value)
}

fn split_tokens(input: &str) -> Vec<String> {
    input.split_whitespace().map(str::to_string).collect()
}

fn ethtool_family_short(family: &str) -> Option<&'static str> {
    match normalize_family_token(family).as_str() {
        "k" | "offload" | "features" => Some("-K"),
        "c" | "coalesce" => Some("-C"),
        "a" | "pause" => Some("-A"),
        "g" | "ring" => Some("-G"),
        "l" | "channels" | "channel" => Some("-L"),
        "x" | "rxfh" | "rss" => Some("-X"),
        "n" | "ntuple" | "configntuple" => Some("-N"),
        _ => None,
    }
}

fn is_ethtool_family_token(token: &str) -> bool {
    ethtool_family_short(token).is_some()
}

fn normalize_family_token(token: &str) -> String {
    token
        .chars()
        .filter(|value| *value != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn run_command(program: &str, args: &[String]) -> Option<(bool, String, i32)> {
    let output = Command::new(program).args(args).output().ok()?;
    let rc = output.status.code().unwrap_or(1);

    let mut merged = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !merged.is_empty() && !merged.ends_with('\n') {
            merged.push('\n');
        }
        merged.push_str(&stderr);
    }

    Some((output.status.success(), merged, rc))
}

fn render_command(program: &str, args: &[String]) -> String {
    if args.is_empty() {
        return program.to_string();
    }
    format!("{program} {}", args.join(" "))
}

fn parse_mac(value: &str) -> Option<[u8; 6]> {
    // strict hex: 6 octets
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.len() != 6 {
        return None;
    }

    let mut octets = [0_u8; 6];
    for (slot, part) in octets.iter_mut().zip(parts) {
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    Some(octets)
}

fn format_mac(octets: [u8; 6]) -> String {
    octets
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn is_hex_mac(value: &str) -> bool {
    let Some(octets) = parse_mac(value) else {
        return false;
    };

    // Reject all-zero / broadcast sentinels.
    if octets.iter().all(|value| *value == 0) {
        return false;
    }
    if octets.iter().all(|value| *value == 0xff) {
        return false;
    }

    // Multicast bit check (LSB of first octet): reject non-unicast MACs.
    octets[0] & 1 == 0
}

fn sanitize_llaa_unicast(value: &str) -> Option<String> {
    let mut octets = parse_mac(value)?;
    octets[0] = (octets[0] | 0x02) & 0xfe;
    Some(format_mac(octets))
}

fn derive_mac_prefix_from(value: &str, pfid: u8) -> Option<String> {
    let mut octets = parse_mac(value)?;
    // Set LAA, clear multicast.
    octets[0] = (octets[0] | 0x02) & 0xfe;
    // Embed PF function nibble.
    octets[4] = (octets[4] & 0xf0) | (pfid & 0x0f);
    Some(format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:",
        octets[0], octets[1], octets[2], octets[3], octets[4]
    ))
}

fn normalize_on_off(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => "on".into(),
        "0" | "false" | "no" | "off" => "off".into(),
        _ => value.to_string(),
    }
}

fn trim_np_suffix(name: &str) -> &str {
    name.rfind("np").map(|index| &name[..index]).unwrap_or(name)
}

fn parse_vf_admin_mac(output: &str, idx: usize) -> Option<String> {
    let expected = idx.to_string();

    for line in output.lines() {
        let mut parts = line.split_whitespace();
        if parts.next()? != "vf" {
            continue;
        }

        let vf_token = parts
            .next()?
            .trim_end_matches(|value: char| matches!(value, ':' | ','));
        if vf_token != expected {
            continue;
        }

        let mut previous = "";
        for token in parts {
            if previous == "MAC" {
                return Some(
                    token
                        .trim_end_matches(|value: char| matches!(value, ':' | ','))
                        .to_string(),
                );
            }
            previous = token;
        }
    }

    None
}

// ---------- logging ----------
// Use stderr with systemd log-level prefixes (<6>=info, <4>=warning).
// Under systemd, stderr is captured via pipe. Standalone, the <N> prefix is harmless.
fn log_info(message: &str) {
    eprintln!("<6>i40e-postlink: {message}");
}

fn log_warn(message: &str) {
    eprintln!("<4>i40e-postlink: {message}");
}

#[cfg(test)]
mod tests {
    use super::{
        derive_mac_prefix_from, normalize_on_off, parse_args, parse_vf_admin_mac,
        sanitize_llaa_unicast, trim_np_suffix, CliToggles, ParseOutcome,
    };

    #[test]
    fn parse_args_accepts_positional_interface_and_toggle_switches() {
        let outcome = parse_args(
            vec![
                "--dry-run".to_string(),
                "--no-vlan-quirk".to_string(),
                "enp2s0f0np0".to_string(),
            ],
            "i40e-postlink",
        )
        .expect("parse should succeed");

        assert_eq!(
            outcome,
            ParseOutcome::Run(super::CliOptions {
                ifname: "enp2s0f0np0".to_string(),
                toggles: CliToggles {
                    dry_run: Some(true),
                    vlan_quirk: Some(false),
                    ..CliToggles::default()
                },
            })
        );
    }

    #[test]
    fn parse_args_accepts_interface_switch() {
        let outcome = parse_args(
            vec!["--interface".to_string(), "enp2s0f1np1".to_string()],
            "i40e-postlink",
        )
        .expect("parse should succeed");

        assert_eq!(
            outcome,
            ParseOutcome::Run(super::CliOptions {
                ifname: "enp2s0f1np1".to_string(),
                toggles: CliToggles::default(),
            })
        );
    }

    #[test]
    fn parse_args_returns_help_text() {
        let outcome =
            parse_args(vec!["--help".to_string()], "i40e-postlink").expect("help should parse");

        assert!(matches!(outcome, ParseOutcome::Help(_)));
    }

    #[test]
    fn sanitize_mac_sets_laa_and_clears_multicast() {
        assert_eq!(
            sanitize_llaa_unicast("01:11:22:33:44:55").as_deref(),
            Some("02:11:22:33:44:55")
        );
    }

    #[test]
    fn derive_prefix_embeds_pf_function_nibble() {
        assert_eq!(
            derive_mac_prefix_from("00:11:22:33:44:55", 0x0b).as_deref(),
            Some("02:11:22:33:4b:")
        );
    }

    #[test]
    fn normalize_query_rss_variants() {
        assert_eq!(normalize_on_off("true"), "on");
        assert_eq!(normalize_on_off("0"), "off");
        assert_eq!(normalize_on_off("auto"), "auto");
    }

    #[test]
    fn trim_np_suffix_matches_shell_pattern() {
        assert_eq!(trim_np_suffix("enp2s0f0np0"), "enp2s0f0");
        assert_eq!(trim_np_suffix("eth0"), "eth0");
    }

    #[test]
    fn parse_vf_admin_mac_accepts_both_ip_formats() {
        let without_colon = "    vf 3 MAC aa:bb:cc:dd:ee:ff, spoof checking off\n";
        let with_colon = "    vf 3: MAC aa:bb:cc:dd:ee:01, spoof checking off\n";

        assert_eq!(
            parse_vf_admin_mac(without_colon, 3).as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(
            parse_vf_admin_mac(with_colon, 3).as_deref(),
            Some("aa:bb:cc:dd:ee:01")
        );
    }
}
