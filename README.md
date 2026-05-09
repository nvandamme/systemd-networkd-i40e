# i40e / X710 SR-IOV Post-link Systemd Service

Post-link helper and systemd service for Intel X710 (i40e/iavf).
> Applies PF/VF ethtool and ip tunables after link/VF creation, supports derived VF MACs, and implements i40e quirks for VLAN tag stripping and host-path asymmetry.

The repository now contains both the original shell helper and a Rust port. The Rust binary keeps the same service contract and command sequence, but moves parsing, VF ordering, and state derivation into a typed implementation.

## How it works

The service runs **once per PF** (e.g., `enp2s0f0np0`) and:

1. Reads **custom properties** from the PF’s `.link` file via `udevadm info -q property`, merges them with runtime environment variables, and then applies any CLI switches. Precedence is **CLI > env > `.link` > default**.
2. **Applies ethtool** families first (PF, then per-VF if the VF netdev is resolvable).
3. **Applies ip(8)** tunables (PF/VF), including all supported PF and VF options, VF trust/spoofchk/admin MAC, and all documented VF subcommands.
4. Optionally **derives VF MACs** from PF MAC (LAA+unicast-safe).
5. Implements **quirks** for per VF VLAN stripping and host-path asymmetry.
6. **DRY_RUN mode:** If `DRY_RUN=1` is set, the script logs all commands it would run, without making changes. It also logs all preset `I40E_*` environment variables.

> The helper **logs every command** (success and failure) to stderr with systemd log-level prefixes (`<6>` info, `<4>` warning). Under systemd, output is captured via the unit's stdio pipe for reliable journal attribution; the `SyslogIdentifier=i40e-postlink` directive sets the journal tag. When run standalone, output goes to the terminal.

## Contents

- Build and install targets: [`Makefile`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/Makefile)
- Rust port: [`src/main.rs`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/src/main.rs)
- Legacy POSIX /bin/sh helper: [`i40e-postlink`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/i40e-postlink)
- Dry-run parity harness: [`tests/compare-dry-run.sh`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/tests/compare-dry-run.sh)
- systemd unit template: [`i40e-postlink@.service`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/i40e-postlink%40.service)
- example systemd-networkd `.link` files:
  - [`10-i40e-pf0.link`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/10-i40e-pf0.link)
  - [`10-i40e-pf1.link`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/10-i40e-pf1.link)

## Setup

### Requirements

- Kernel drivers: `i40e` (PF), `iavf` (VF).
- Tools: `iproute2`, `ethtool`, `systemd-udev`, `systemd-networkd`.
- Rust toolchain (`cargo`, `rustc`) if you build the Rust port from source.
- SR-IOV enabled in BIOS and available on the NIC.
- VFs created either via .link SR-IOV* keys or by other means (udev rules, sysfs echo, etc.) before the service runs.

### Build

The repository includes a `Makefile` for both variants:

```bash
make build
make build-shell
make build-all
```

- `make build` compiles `target/release/i40e-postlink` and is the default Make target.
- `make build-shell` validates the shell helper syntax.
- `make build-all` does both.

Optional local validation before install:

```bash
make test
make parity
```

If you only want the raw Cargo build step, it is still:

```bash
cargo build --release
```

The compiled Rust binary is written to `target/release/i40e-postlink`.

### Install

> [!WARNING]
> Make sure to edit the `.link` files to match your PF PCI addresses, desired VF counts, and any custom tunables you want to apply.
---
> [!IMPORTANT]
> Example systemd-networkd `.link` files to edit before installing:
> [`10-i40e-pf0.link`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/10-i40e-pf0.link)
> and [`10-i40e-pf1.link`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/10-i40e-pf1.link)

Build first as your normal user, then install and activate as root.

Install the Rust variant:

```bash
make build
sudo make install
sudo make activate PF_INTERFACES="enp2s0f0np0 enp2s0f1np1"
```

Install the shell fallback variant:

```bash
make build-shell
sudo make install-shell
sudo make activate PF_INTERFACES="enp2s0f0np0 enp2s0f1np1"
```

The `activate` target does the runtime side of the install:

- `systemctl daemon-reload`
- `udevadm control --reload`
- `udevadm trigger --action=add /sys/class/net/<pf>` for each PF in `PF_INTERFACES`
- `systemctl enable --now i40e-postlink@<pf>.service` for each PF in `PF_INTERFACES`

If you want a single command after the build step, use the convenience targets:

```bash
make build
sudo make install-live PF_INTERFACES="enp2s0f0np0 enp2s0f1np1"

make build-shell
sudo make install-shell-live PF_INTERFACES="enp2s0f0np0 enp2s0f1np1"
```

For packaging or staged installs, the Makefile also supports `DESTDIR` without activation:

```bash
make build
make DESTDIR=/tmp/i40e-postlink-root install
```

If you are installing on a fresh host, a reboot is also sufficient after copying the `.link` files and unit.

After install, verify the active helper and the service logs:

```bash
/usr/local/sbin/i40e-postlink --help
journalctl -u i40e-postlink@enp2s0f0np0.service -b
```

## Runtime toggles

- `DRY_RUN` or `--dry-run` / `--no-dry-run` → simulate commands without mutating state, default **off**
- `DERIVE_VF_MACS` or `--derive-vf-macs` / `--no-derive-vf-macs` → derive VF MACs from PF MAC + VF index, default **on**
- `SKIP_IF_VF_MAC_SET` or `--skip-if-vf-mac-set` / `--no-skip-if-vf-mac-set` → skip setting a derived MAC if an admin MAC already exists, default **on**
- `ENABLE_VF_TRUE_PROMISC` or `--enable-vf-true-promisc` / `--no-enable-vf-true-promisc` → enable full promiscuous support for VFs when the PF priv-flag exists, default **on**
- `DISABLE_FW_LLDP` or `--disable-fw-lldp` / `--no-disable-fw-lldp` → disable the Intel firmware LLDP agent when the PF priv-flag exists, default **on**
- `ASYM_QUIRK` or `--asym-quirk` / `--no-asym-quirk` → PF TX checksum disable + symmetric coalesce workaround for host-path asymmetry, default **on**
- `VLAN_QUIRK` or `--vlan-quirk` / `--no-vlan-quirk` → workaround the VF VLAN tag stripping bug, default **on**

> [!IMPORTANT]
> The shell helper and Rust binary support the same toggle switches. Resolution order is `CLI > env > .link Property=I40E_<NAME> > built-in default`.

## DRY_RUN mode

- Set `DRY_RUN=1` in the environment or pass `--dry-run` to simulate all actions.
- The helper will log all commands it would run (prefixed with `DRY:`) and will not mutate device state.
- It will also log all preset `I40E_*` environment variables for debugging.

**Example:**

```bash
DRY_RUN=1 I40E_PF_OFFLOAD="gro off" /usr/local/sbin/i40e-postlink --dry-run --no-vlan-quirk enp2s0f0np0
# Output:
# DRY_RUN: Listing preset I40E_* environment variables:
# DRY_RUN: I40E_PF_OFFLOAD="gro off"
# DRY: ethtool -K enp2s0f0np0 gro off
...
```

## Parity testing

- Run `sh tests/compare-dry-run.sh --fixture` for a host-independent shell vs Rust parity check. This mode creates a fake `sysfs` tree, fake `udevadm`/`ip`/`ethtool` commands, and exercises a synthetic PF with two VFs.
- Run `sh tests/compare-dry-run.sh <pf-ifname>` to compare both implementations against a real PF on the current host.
- The fixture mode is the recommended default, because it covers VF-path behavior without requiring SR-IOV hardware on the current host.

### Alternate sysfs root

- `I40E_SYS_CLASS_NET_ROOT` overrides the default `/sys/class/net` lookup root for both the shell helper and the Rust binary.
- This is intended for fixture testing and controlled dry-run comparisons, not for normal production installs.

**Examples:**

```bash
sh tests/compare-dry-run.sh --fixture
sh tests/compare-dry-run.sh enp2s0f0np0
I40E_SYS_CLASS_NET_ROOT=/tmp/fake-sys/class/net DRY_RUN=1 /usr/local/sbin/i40e-postlink testpf0
```

## Property reference (for systemd-networkd `.link` files via `Property=`)

> [!WARNING]
> **Do not** include `-K/-C/-A/...` switches in property values; the script maps each property to the right ethtool family and applies the whole set **in one operation**.
---
> [!CAUTION]
> All properties must be quoted if they contain spaces, e.g.:
> `Property=I40E_PF_OFFLOAD="rxvlan off tx-checksum-ip-generic off"`

### PF-level properties

| Property key           | Maps to                             | Example value(s)                                                 |
| ---------------------- | ----------------------------------- | ---------------------------------------------------------------- |
| `I40E_PF_OFFLOAD`      | `ethtool -K <PF>`                   | `rxvlan off` · `tx-checksum-ip-generic off tx-checksum-sctp off` |
| `I40E_PF_COALESCE`     | `ethtool -C <PF>`                   | `adaptive-rx off adaptive-tx off rx-usecs 0 tx-usecs 0`          |
| `I40E_PF_PAUSE`        | `ethtool -A <PF>`                   | `rx off tx off`                                                  |
| `I40E_PF_RINGS`        | `ethtool -G <PF>`                   | `rx 4096 tx 4096`                                                |
| `I40E_PF_CHANNELS`     | `ethtool -L <PF>`                   | `combined 8`                                                     |
| `I40E_PF_RSS`          | `ethtool -X <PF>`                   | `hfunc toeplitz` (kern ≥ 6.8 defaults sane; usually not needed)  |
| `I40E_PF_NTUPLE`       | `ethtool -N <PF>`                   | `rx-flow-hash tcp4 sdfn`                                         |
| `I40E_PRIVFLAGS`       | `ethtool --set-priv-flags <PF>`     | `disable-fw-lldp on vf-true-promisc-support on`                  |
| `I40E_PF_TXQUEUELEN`   | `ip link set dev <PF> txqueuelen`   | `10000`                                                          |
| `I40E_PF_MTU`          | `ip link set dev <PF> mtu`          | `9000`                                                           |
| `I40E_PF_ALIAS`        | `ip link set dev <PF> alias`        | `pf0`                                                            |
| `I40E_PF_ARP`          | `ip link set dev <PF> arp`          | `on`/`off`                                                       |
| `I40E_PF_MULTICAST`    | `ip link set dev <PF> multicast`    | `on`/`off`                                                       |
| `I40E_PF_ALLMULTICAST` | `ip link set dev <PF> allmulticast` | `on`/`off`                                                       |
| `I40E_PF_PROMISC`      | `ip link set dev <PF> promisc`      | `on`/`off`                                                       |
| `I40E_PF_MAC`          | `ip link set dev <PF> address`      | `aa:bb:cc:dd:ee:ff`                                              |

### Global toggles / quirks (PF-scoped booleans)

| Property key                  | Default | Effect                                                                                                                                   |
| ----------------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `I40E_DISABLE_FW_LLDP`        | `1`     | Tries `disable-fw-lldp on` PF priv-flag if supported.                                                                                    |
| `I40E_ENABLE_VF_TRUE_PROMISC` | `1`     | Tries `vf-true-promisc-support on` PF priv-flag if supported (helps all-multicast on VFs).                                               |
| `I40E_DERIVE_VF_MACS`         | `1`     | Derive VF MACs from PF MAC + VF index; LAA bit set; unicast enforced.                                                                    |
| `I40E_SKIP_IF_VF_MAC_SET`     | `1`     | Preserve existing admin MACs configured elsewhere (libvirt/cloud/etc.).                                                                  |
| `I40E_ASYM_QUIRK`             | `1`     | **Host-path asymmetry mitigation**: on PF only - `-C adaptive off ... usecs 0` and `-K tx-checksum-ip-generic off tx-checksum-sctp off`. |
| `I40E_VLAN_QUIRK`             | `1`     | **VLAN tag quirk**: ensure `rxvlan off` on PF and VF, then `ip link set PF vf N vlan 10` followed by `... vlan 0`.                       |

### Per-VF properties (index `N`)

> **Global VF fallbacks:** every per-VF property `I40E_VF<N>_<KEY>` has a corresponding global form `I40E_VF_<KEY>` (e.g. `I40E_VF_OFFLOAD`, `I40E_VF_COALESCE`, `I40E_VF_TXQUEUELEN`) that is applied to **all** VFs unless overridden by the per-VF variant. VF subcommands via the PF handle (`SPOOFCHK`, `TRUST`, `RATE`, etc.) also have global fallbacks.

| Property key pattern      | Maps to                             | Example                                                                 |
| ------------------------- | ----------------------------------- | ----------------------------------------------------------------------- |
| `I40E_VF<N>_OFFLOAD`      | `ethtool -K <VF>`                   | `rxvlan off`                                                            |
| `I40E_VF<N>_COALESCE`     | `ethtool -C <VF>`                   | `rx-usecs 0 tx-usecs 0`                                                 |
| `I40E_VF<N>_PAUSE`        | `ethtool -A <VF>`                   | `rx off tx off`                                                         |
| `I40E_VF<N>_RINGS`        | `ethtool -G <VF>`                   | `rx 1024 tx 1024`                                                       |
| `I40E_VF<N>_CHANNELS`     | `ethtool -L <VF>`                   | `combined 2`                                                            |
| `I40E_VF<N>_RSS`          | `ethtool -X <VF>`                   | `hfunc toeplitz`                                                        |
| `I40E_VF<N>_NTUPLE`       | `ethtool -N <VF>`                   | `rx-flow-hash udp4 sdfn`                                                |
| `I40E_VF<N>_MAC`          | `ip link set PF vf N mac`           | `aa:bb:cc:dd:ee:ff` (validated, normalized to LAA+unicast)              |
| `I40E_VF<N>_VLAN_QUIRK`   | VF quirk flag                       | `1` to enforce VLAN bounce for that VF (in addition to global)          |
| `I40E_VF<N>_ASYM_QUIRK`   | parsed/logged only                  | `1` logs a note; **no VF-side action** (quirk is PF/host-path specific) |
| `I40E_VF<N>_TXQUEUELEN`   | `ip link set dev <VF> txqueuelen`   | `10000`                                                                 |
| `I40E_VF<N>_MTU`          | `ip link set dev <VF> mtu`          | `9000`                                                                  |
| `I40E_VF<N>_ALIAS`        | `ip link set dev <VF> alias`        | `vf3`                                                                   |
| `I40E_VF<N>_ARP`          | `ip link set dev <VF> arp`          | `on`/`off`                                                              |
| `I40E_VF<N>_MULTICAST`    | `ip link set dev <VF> multicast`    | `on`/`off`                                                              |
| `I40E_VF<N>_ALLMULTICAST` | `ip link set dev <VF> allmulticast` | `on`/`off`                                                              |
| `I40E_VF<N>_PROMISC`      | `ip link set dev <VF> promisc`      | `on`/`off`                                                              |

#### VF subcommands (via PF handle)

| Property key pattern     | Maps to                                 | Example            |
| ------------------------ | --------------------------------------- | ------------------ |
| `I40E_VF<N>_VLAN`        | `ip link set dev <PF> vf N vlan`        | `10`               |
| `I40E_VF<N>_RATE`        | `ip link set dev <PF> vf N rate`        | `1000`             |
| `I40E_VF<N>_SPOOFCHK`    | `ip link set dev <PF> vf N spoofchk`    | `on`/`off`         |
| `I40E_VF<N>_STATE`       | `ip link set dev <PF> vf N state`       | `enable`/`disable` |
| `I40E_VF<N>_QUERY_RSS`   | `ip link set dev <PF> vf N query_rss`   | `on`/`off`         |
| `I40E_VF<N>_TRUST`       | `ip link set dev <PF> vf N trust`       | `on`/`off`         |
| `I40E_VF<N>_MAX_TX_RATE` | `ip link set dev <PF> vf N max_tx_rate` | `10000`            |
| `I40E_VF<N>_MIN_TX_RATE` | `ip link set dev <PF> vf N min_tx_rate` | `1000`             |

> **`query_rss` auto-enable:** when ethtool RSS tunables are set (`I40E_VF<N>_RSS` or `I40E_VF_RSS`) but no explicit `I40E_VF<N>_QUERY_RSS` is provided, `query_rss on` is issued automatically via the PF handle.
>
> **`query_rss` value normalization:** `1`/`true`/`yes`/`on` → `on`; `0`/`false`/`no`/`off` → `off`.

---

## Quirks explained (with references)

> [!IMPORTANT]
> **VLAN tag stripping on SR-IOV**:
> Some X710/i40e/iavf combinations strip incoming 802.1Q tags for VFs.
> **Mitigation:** turn **off** VLAN offload (`rxvlan off`) on PF and VF, then **bounce** a temporary port VLAN.
> Reference: <https://community.intel.com/t5/Ethernet-Products/X710-strips-incoming-vlan-tag-with-SRIOV/m-p/551464>
---
> [!IMPORTANT]
> **Asymmetric host-path throughput**:
> One direction (typically upload vs. download) performs significantly worse through the host/bridge path.
> **Mitigation:** keep coalesce symmetric & non-adaptive, and **disable PF hardware TX checksumming** (`tx-checksum-ip-generic` and `tx-checksum-sctp`).
> Reference: <https://community.intel.com/t5/Ethernet-Products/Intel-X710-SFP-Asymmetric-Performance-Issue-Upload-Download/m-p/1685603>
---
> [!NOTE]
> On kernels ≥ **6.8**, `i40e`’s defaults for **channels** (combined ≈ CPU count) and **RSS Toeplitz** are already good; we **do not** force `-L` or `-X` unless you explicitly configure them.

---

## Derived VF MACs

- Computed from PF MAC with:
  - **LAA bit set**, multicast bit **cleared**.
  - PF PCI **function nibble** embedded in octet 5.
  - VF index as last octet (`%02x`).
- Guardrails:
  - Reject all-zero / all-FF / multicast MACs.
  - Respect `I40E_SKIP_IF_VF_MAC_SET=1`.

---

## Logging, verification & troubleshooting

### Logs

```bash
journalctl -u i40e-postlink@enp2s0f0np0.service -b
```

### Verify

```bash
ethtool --show-priv-flags enp2s0f0np0
ethtool -k enp2s0f0np0 | egrep 'rx-vlan|tx-checksum|tx-checksum-(ip|ipv|sctp)'
ip -d link show enp2s0f0np0
```

### Common pitfalls

- A property uses a switch (e.g., `-K`) — **remove the switch**; give only the arguments.
- A flag isn’t supported on your FW/kernel — the script logs a **WARNING** and continues.
- VF netdev name not yet present — per-VF **ethtool** and VF-netdev ip tunables are skipped (a **warning** is logged); PF-handle VF subcommands (`spoofchk`, `trust`, `vlan`, `rate`, `state`, `query_rss`, `max_tx_rate`, `min_tx_rate`) still run because they operate via `ip link set dev <PF> vf N ...`.
- In DRY_RUN mode, no changes are made; only logs are produced.

---

## Security & scope

- The helper mutates **network device state**; run with root privileges only.
- Changes are **local to the host PF/VFs** being configured.

---

## License & authorship

- Copyright (c) 2025 **Nicolas Vandamme**
- Licensed under the **MIT License**

## Notes

- `vf-vlan-pruning` is unrelated to the VLAN tag stripping bug; leave it off unless you want VLAN filtering policy.
- The script tolerates long/short family names and even accidental leading family tokens in args.
- `apply_ip_tunables` auto-detects PF vs VF by whether the second token is numeric.
