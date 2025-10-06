# i40e / X710 SR-IOV Post-link Systemd Service

Post-link helper script and systemd service for Intel X710 (i40e/iavf).
> Applies PF/VF ethtool and ip tunables after link/VF creation, supports derived VF MACs, and implements i40e quirks for VLAN tag stripping and host-path asymmetry.

## How it works

The service runs **once per PF** (e.g., `enp2s0f0np0`) and:

1. Reads **custom properties** from the PF’s `.link` file via `udevadm info -q property` and merges with any preset `I40E_*` environment variables (env vars take precedence).
2. **Applies ethtool** families first (PF, then per-VF if the VF netdev is resolvable).
3. **Applies ip(8)** tunables (PF/VF), including all supported PF and VF options, VF trust/spoofchk/admin MAC, and all documented VF subcommands.
4. Optionally **derives VF MACs** from PF MAC (LAA+unicast-safe).
5. Implements **quirks** for per VF VLAN stripping and host-path asymmetry.
6. **DRY_RUN mode:** If `DRY_RUN=1` is set, the script logs all commands it would run, without making changes. It also logs all preset `I40E_*` environment variables.

> The helper **logs every command** (success and failure) via `logger -t i40e-postlink -p kern.*`.

## Contents

- POSIX /bin/sh script, place in `/usr/local/sbin/`: [`i40e-postlink`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/i40e-postlink)
- systemd unit template: [`i40e-postlink@.service`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/i40e-postlink%40.service)
- example systemd-networkd `.link` files:
  - [`10-i40e-pf0.link`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/10-i40e-pf0.link)
  - [`10-i40e-pf1.link`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/10-i40e-pf1.link)

## Setup

### Requirements

- Kernel drivers: `i40e` (PF), `iavf` (VF).
- Tools: `iproute2`, `ethtool`, `systemd-udev`, `systemd-networkd`, `logger`.
- SR-IOV enabled in BIOS and available on the NIC.
- VFs created either via .link SR-IOV* keys or by other means (udev rules, sysfs echo, etc.) before the service runs.

### Install

> [!WARNING]
> Make sure to edit the `.link` files to match your PF PCI addresses, desired VF counts, and any custom tunables you want to apply.
>
> Then install the script and service, reload systemd, and enable/start the service instances for your PFs.

> [!IMPORTANT]
> Example systemd-networkd `.link` files to edit+copy to `/etc/systemd/network/` **before enabling the service**
>
> - [`10-i40e-pf0.link`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/10-i40e-pf0.link)
> - [`10-i40e-pf1.link`](https://github.com/nvandamme/systemd-networkd-i40e/blob/main/10-i40e-pf1.link)

```bash
sudo install -m 0755 i40e-postlink /usr/local/sbin/i40e-postlink
sudo install -m 0644 i40e-postlink@.service /etc/systemd/system/i40e-postlink@.service
sudo systemctl daemon-reload
sudo systemctl enable --now i40e-postlink@enp2s0f0np0.service
sudo systemctl enable --now i40e-postlink@enp2s0f1np1.service
```

## Supported PF and VF ip link tunables

The script supports all major `ip link` tunables for PF and VF, including:

- **PF-level:** `txqueuelen`, `mtu`, `alias`, `arp`, `multicast`, `allmulticast`, `promisc`, `mac` (address)
- **VF-level (to VF netdev):** `txqueuelen`, `mtu`, `alias`, `arp`, `multicast`, `allmulticast`, `promisc`, `mac` (address)
- **VF subcommands (via PF handle):** `mac`, `vlan`, `rate`, `spoofchk`, `state`, `query_rss`, `trust`, `max_tx_rate`, `min_tx_rate`, `rss`

Set these via environment variables or `.link` properties, e.g.:

- `I40E_PF_TXQUEUELEN=10000`
- `I40E_VF_MTU=9000`
- `I40E_VF3_PROMISC=on`
- `I40E_VF2_VLAN=10`

## Environment defaults (can be overridden via `.link` Properties or preset as env vars)

- `DERIVE_VF_MACS`           → derive VF MACs from PF MAC + VF index, default **on**
- `SKIP_IF_VF_MAC_SET`       → skip setting derived MAC if admin MAC already set, default **on**
- `ENABLE_VF_TRUE_PROMISC`   → enable full promiscuous for VFs (ensures allmulticast), default **on**
- `DISABLE_FW_LLDP`          → disable Intel FW LLDP agent, default **on**
- `ASYM_QUIRK`               → PF TX csum off + symmetric coalesce (host-path asymmetry), default **on**
- `VLAN_QUIRK`               → workaround VLAN tag stripping bug, default **on**

> [!IMPORTANT]
> All can be overridden via `.link` file with corresponding `Property=I40E_<NAME>=0|1`, or by setting the environment variable before invoking the script. Environment variables take precedence over `.link` properties.

## DRY_RUN mode

- Set `DRY_RUN=1` in the environment to simulate all actions.
- The script will log all commands it would run (prefixed with `DRY:`) and will not mutate device state.
- It will also log all preset `I40E_*` environment variables for debugging.

**Example:**

```bash
DRY_RUN=1 I40E_PF_OFFLOAD="gro off" /usr/local/sbin/i40e-postlink enp2s0f0np0
# Output:
# DRY_RUN: Listing preset I40E_* environment variables:
# DRY_RUN: I40E_PF_OFFLOAD=gro off
# DRY: ethtool -K enp2s0f0np0 gro off
...
```

## Property reference (for systemd-networkd `.link` files via `Property=`)

> [!WARNING]
> **Do not** include `-K/-C/-A/...` switches in property values; the script maps each property to the right ethtool family and applies the whole set **in one operation**.

### PF-level properties

| Property key                | Maps to                 | Example value(s)                                                                 |
|----------------------------|-------------------------|----------------------------------------------------------------------------------|
| `I40E_PF_OFFLOAD`          | `ethtool -K <PF>`       | `rxvlan off` · `tx-checksum-ip-generic off tx-checksum-sctp off`                |
| `I40E_PF_COALESCE`         | `ethtool -C <PF>`       | `adaptive-rx off adaptive-tx off rx-usecs 0 tx-usecs 0`                         |
| `I40E_PF_PAUSE`            | `ethtool -A <PF>`       | `rx off tx off`                                                                 |
| `I40E_PF_RINGS`            | `ethtool -G <PF>`       | `rx 4096 tx 4096`                                                               |
| `I40E_PF_CHANNELS`         | `ethtool -L <PF>`       | `combined 8`                                                                     |
| `I40E_PF_RSS`              | `ethtool -X <PF>`       | `hfunc toeplitz` (kern ≥ 6.8 defaults sane; usually not needed)                 |
| `I40E_PF_NTUPLE`           | `ethtool -N <PF>`       | `rx-flow-hash tcp4 sdfn`                                                         |
| `I40E_PRIVFLAGS`           | `ethtool --set-priv-flags <PF>` | `disable-fw-lldp on vf-true-promisc-support on`                        |
| `I40E_PF_TXQUEUELEN`     | `ip link set dev <PF> txqueuelen` | `10000`                                   |
| `I40E_PF_MTU`           | `ip link set dev <PF> mtu`        | `9000`                                    |
| `I40E_PF_ALIAS`         | `ip link set dev <PF> alias`      | `pf0`                                     |
| `I40E_PF_ARP`           | `ip link set dev <PF> arp`        | `on`/`off`                                |
| `I40E_PF_MULTICAST`     | `ip link set dev <PF> multicast`  | `on`/`off`                                |
| `I40E_PF_ALLMULTICAST`  | `ip link set dev <PF> allmulticast` | `on`/`off`                              |
| `I40E_PF_PROMISC`       | `ip link set dev <PF> promisc`    | `on`/`off`                                |
| `I40E_PF_MAC`           | `ip link set dev <PF> address`    | `aa:bb:cc:dd:ee:ff`                       |

### Global toggles / quirks (PF-scoped booleans)

| Property key                    | Default | Effect |
|--------------------------------|---------|--------|
| `I40E_DISABLE_FW_LLDP`         | `1`     | Tries `disable-fw-lldp on` PF priv-flag if supported. |
| `I40E_ENABLE_VF_TRUE_PROMISC`  | `1`     | Tries `vf-true-promisc-support on` PF priv-flag if supported (helps all-multicast on VFs). |
| `I40E_DERIVE_VF_MACS`          | `1`     | Derive VF MACs from PF MAC + VF index; LAA bit set; unicast enforced. |
| `I40E_SKIP_IF_VF_MAC_SET`      | `1`     | Preserve existing admin MACs configured elsewhere (libvirt/cloud/etc.). |
| `I40E_ASYM_QUIRK`              | `1`     | **Host-path asymmetry mitigation**: on PF only — `-C adaptive off ... usecs 0` and `-K tx-checksum-ip-generic off tx-checksum-sctp off`. |
| `I40E_VLAN_QUIRK`              | `1`     | **VLAN tag quirk**: ensure `rxvlan off` on PF & VF, then `ip link set PF vf N vlan 10` followed by `... vlan 0`. |

### Per-VF properties (index `N`)

| Property key pattern                 | Maps to                      | Example |
|-------------------------------------|------------------------------|---------|
| `I40E_VF<N>_OFFLOAD`                | `ethtool -K <VF>`            | `rxvlan off` |
| `I40E_VF<N>_PAUSE`                  | `ethtool -A <VF>`            | `rx off tx off` |
| `I40E_VF<N>_RINGS`                  | `ethtool -G <VF>`            | `rx 1024 tx 1024` |
| `I40E_VF<N>_CHANNELS`               | `ethtool -L <VF>`            | `combined 2` |
| `I40E_VF<N>_RSS`                    | `ethtool -X <VF>`            | `hfunc toeplitz` |
| `I40E_VF<N>_NTUPLE`                 | `ethtool -N <VF>`            | `rx-flow-hash udp4 sdfn` |
| `I40E_VF<N>_MAC`                    | `ip link set PF vf N mac`    | `aa:bb:cc:dd:ee:ff` (validated, normalized to LAA+unicast) |
| `I40E_VF<N>_VLAN_QUIRK`             | VF quirk flag                | `1` to enforce VLAN bounce for that VF (in addition to global) |
| `I40E_VF<N>_ASYM_QUIRK`             | parsed/logged only           | `1` logs a note; **no VF-side action** (quirk is PF/host-path specific) |
| `I40E_VF<N>_TXQUEUELEN`           | `ip link set dev <VF> txqueuelen` | `10000` |
| `I40E_VF<N>_MTU`                 | `ip link set dev <VF> mtu`        | `9000`  |
| `I40E_VF<N>_ALIAS`               | `ip link set dev <VF> alias`      | `vf3`   |
| `I40E_VF<N>_ARP`                 | `ip link set dev <VF> arp`        | `on`/`off` |
| `I40E_VF<N>_MULTICAST`           | `ip link set dev <VF> multicast`  | `on`/`off` |
| `I40E_VF<N>_ALLMULTICAST`        | `ip link set dev <VF> allmulticast` | `on`/`off` |
| `I40E_VF<N>_PROMISC`             | `ip link set dev <VF> promisc`    | `on`/`off` |

#### VF subcommands (via PF handle)

| Property key pattern                 | Maps to                      | Example |
|-------------------------------------|------------------------------|---------|
| `I40E_VF<N>_VLAN`                | `ip link set dev <PF> vf N vlan`        | `10` |
| `I40E_VF<N>_RATE`                | `ip link set dev <PF> vf N rate`        | `1000` |
| `I40E_VF<N>_SPOOFCHK`            | `ip link set dev <PF> vf N spoofchk`    | `on`/`off` |
| `I40E_VF<N>_STATE`               | `ip link set dev <PF> vf N state`       | `enable`/`disable` |
| `I40E_VF<N>_QUERY_RSS`           | `ip link set dev <PF> vf N query_rss`   | `on`/`off` |
| `I40E_VF<N>_TRUST`               | `ip link set dev <PF> vf N trust`       | `on`/`off` |
| `I40E_VF<N>_MAX_TX_RATE`         | `ip link set dev <PF> vf N max_tx_rate` | `10000` |
| `I40E_VF<N>_MIN_TX_RATE`         | `ip link set dev <PF> vf N min_tx_rate` | `1000` |
| `I40E_VF<N>_RSS`                 | `ip link set dev <PF> vf N rss`         | `on`/`off` |

---

## Quirks explained (with references)

> [!IMPORTANT]
> **VLAN tag stripping on SR-IOV**:
>
> - Some X710/i40e/iavf combinations strip incoming 802.1Q tags for VFs.
> - **Mitigation:** turn **off** VLAN offload (`rxvlan off`) on PF and VF, then **bounce** a temporary port VLAN.
> - Reference: <https://community.intel.com/t5/Ethernet-Products/X710-strips-incoming-vlan-tag-with-SRIOV/m-p/551464>

> [!IMPORTANT]
> **Asymmetric host-path throughput**:
>
> - One direction (typically upload vs. download) performs significantly worse through the host/bridge path.
> - **Mitigation:** keep coalesce symmetric & non-adaptive, and **disable PF hardware TX checksumming** (`tx-checksum-ip-generic` and `tx-checksum-sctp`).
> - Reference: <https://community.intel.com/t5/Ethernet-Products/Intel-X710-SFP-Asymmetric-Performance-Issue-Upload-Download/m-p/1685603>

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
journalctl -t i40e-postlink -b
```

### Verify

```bash
ethtool --show-priv-flags enp2s0f0np0
ethtool -k enp2s0f0np0 | egrep 'rxvlan|tx-checksum|tx-checksum-(ip|ipv|sctp)'
ip -d link show enp2s0f0np0
```

### Common pitfalls

- A property uses a switch (e.g., `-K`) — **remove the switch**; give only the arguments.
- A flag isn’t supported on your FW/kernel — the script logs a **WARNING** and continues.
- VF netdev name not yet present — per-VF **ethtool** might be skipped; IP ops still run on the PF’s VF handle.
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
