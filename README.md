# i40e / X710 SR-IOV Post-link SystemD Service

This packages a post-link helper script and a systemd service for Intel X710 (i40e/iavf). It applies PF/VF tunables right after link/VF creation, supports derived VF MACs, and implements the i40e VM pass-through quirk.

## Contents
- `i40e-postlink` — POSIX /bin/sh script, place in `/usr/local/sbin/`
- `i40e-postlink@.service` — systemd unit template
- `10-i40e-pf0.link`, `11-i40e-pf1.link` — example `.link` files

## Install
```bash
sudo install -m 0755 i40e-postlink /usr/local/sbin/i40e-postlink
sudo install -m 0644 i40e-postlink@.service /etc/systemd/system/i40e-postlink@.service
sudo systemctl daemon-reload
sudo systemctl enable --now i40e-postlink@enp2s0f0np0.service
sudo systemctl enable --now i40e-postlink@enp2s0f1np1.service
```

## Ordering
1. PF ethtool (features/flags/rings/etc.)
2. VF ethtool (per VF if resolvable)
3. IP-level ops (trust/spoofchk/MAC; VLAN quirk bounce)

## Environment defaults (can be overridden via `.link` Properties)
```sh
DERIVE_VF_MACS=${DERIVE_VF_MACS:-1}          # derive VF MACs from PF MAC + VF index, default to on
SKIP_IF_VF_MAC_SET=${SKIP_IF_VF_MAC_SET:-1}  # skip setting derived MAC if admin MAC already set, default to on
ENABLE_VF_TRUE_PROMISC=${ENABLE_VF_TRUE_PROMISC:-1} # full promiscuous for VFs (ensuring allmulticast to work), default to on
QUIRK_VLAN_BOUNCE=${QUIRK_VLAN_BOUNCE:-1}    # VLAN offloading incurs VLAN source stripping bug, default to on
DISABLE_FW_LLDP=${DISABLE_FW_LLDP:-1}        # Intel FW LLDP agent conflicts, default to on
```

## `.link` properties (PF-level)
Give **args only** (no `-K/-C/...`).

- `I40E_PF_OFFLOAD=…`    → `ethtool -K`
- `I40E_PF_COALESCE=…`   → `ethtool -C`
- `I40E_PF_PAUSE=…`      → `ethtool -A`
- `I40E_PF_RINGS=…`      → `ethtool -G`
- `I40E_PF_CHANNELS=…`   → `ethtool -L`
- `I40E_PF_RSS=…`        → `ethtool -X` (alias `rss` supported)
- `I40E_PF_NTUPLE=…`     → `ethtool -N`
- `I40E_PRIVFLAGS="flag on flag2 off ..."` (e.g., `disable-fw-lldp on vf-true-promisc-support on`)

### Global toggles (0/1)
- `I40E_DISABLE_FW_LLDP`, `I40E_ENABLE_VF_TRUE_PROMISC`, `I40E_VM_QUIRK`  
- `I40E_DERIVE_VF_MACS`, `I40E_SKIP_IF_VF_MAC_SET`

## Per-VF properties (index N)
- `I40E_VF<N>_OFFLOAD`, `_PAUSE`, `_RINGS`, `_CHANNELS`, `_RSS`, `_NTUPLE` → same mappings as PF
- `I40E_VF<N>_MAC=aa:bb:cc:dd:ee:ff` (validated & normalized to LAA+unicast)
- `I40E_VF<N>_QUIRK=1` (enforce rxvlan off PF+VF, then VLAN bounce)

## VM quirk (i40e/iavf)
Enable with `I40E_VM_QUIRK=1` (default). The script will:
- `ethtool -K` `rxvlan off` on **PF + VF** first
- `ip link set dev PF vf N vlan 10` then `vlan 0`

## Logging
- INFO on applied commands, WARNING on failures. View: `journalctl -t i40e-postlink -b`

## Example `.link` snippets
See `10-i40e-pf0.link` and `11-i40e-pf1.link` in this tarball for full examples.

## Notes
- `vf-vlan-pruning` is unrelated to the VLAN tag stripping bug; leave it off unless you want VLAN filtering policy.
- The script tolerates long/short family names and even accidental leading family tokens in args.
- `apply_ip_tunables` auto-detects PF vs VF by whether the second token is numeric.
