#!/bin/sh

set -eu

usage() {
  cat <<EOF
usage: $0 <pf-ifname>
       $0 --fixture

Compares DRY_RUN output parity between:
  - ./i40e-postlink
  - ./target/release/i40e-postlink

The script runs two scenarios:
  1. env-driven PF tuning
  2. CLI overrides taking precedence over env defaults

Modes:
  <pf-ifname>  Use a real host PF interface.
  --fixture    Use a host-independent fake sysfs tree and fake commands.
EOF
}

MODE=host
IFNAME=''

case ${1:-} in
  --fixture)
    MODE=fixture
    IFNAME=testpf0
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  '')
    usage >&2
    exit 2
    ;;
  *)
    IFNAME=$1
    ;;
esac

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
SHELL_HELPER=$ROOT_DIR/i40e-postlink
RUST_HELPER=$ROOT_DIR/target/release/i40e-postlink

cleanup() {
  rm -f -- "${TMP_SHELL:-}" "${TMP_RUST:-}"
  rm -rf -- "${FIXTURE_ROOT:-}"
}
trap cleanup EXIT INT TERM HUP

TMP_SHELL=$(mktemp -t i40e-shell-dryrun.XXXXXX)
TMP_RUST=$(mktemp -t i40e-rust-dryrun.XXXXXX)

compare_case() {
  case_name=$1

  if ! diff -u "$TMP_SHELL" "$TMP_RUST"; then
    printf 'dry-run parity failed for case: %s\n' "$case_name" >&2
    exit 1
  fi

  printf 'ok: %s\n' "$case_name"
}

setup_fixture() {
  FIXTURE_ROOT=$(mktemp -d -t i40e-fixture.XXXXXX)
  FIXTURE_SYS_CLASS_NET_ROOT=$FIXTURE_ROOT/sys/class/net
  FIXTURE_BIN_ROOT=$FIXTURE_ROOT/bin

  mkdir -p \
    "$FIXTURE_SYS_CLASS_NET_ROOT/testpf0" \
    "$FIXTURE_SYS_CLASS_NET_ROOT/testpf0v0" \
    "$FIXTURE_SYS_CLASS_NET_ROOT/testpf0v1" \
    "$FIXTURE_ROOT/pci/0000:72:00.0" \
    "$FIXTURE_BIN_ROOT"

  printf '%s\n' '98:b7:85:24:00:11' > "$FIXTURE_SYS_CLASS_NET_ROOT/testpf0/address"
  ln -s "$FIXTURE_ROOT/pci/0000:72:00.0" "$FIXTURE_SYS_CLASS_NET_ROOT/testpf0/device"
  : > "$FIXTURE_ROOT/pci/0000:72:00.0/virtfn0"
  : > "$FIXTURE_ROOT/pci/0000:72:00.0/virtfn1"

  cat > "$FIXTURE_BIN_ROOT/udevadm" <<'EOF'
#!/bin/sh
set -eu

sys_path=''
while [ $# -gt 0 ]; do
  case "$1" in
    -p)
      shift
      sys_path=${1:-}
      ;;
  esac
  shift || break
done

case "$sys_path" in
  */testpf0)
    cat <<'OUT'
I40E_PF_OFFLOAD="gro off"
I40E_PF_COALESCE="rx-usecs 8 tx-usecs 4"
I40E_PF_ALIAS=fixture-pf
I40E_VF_TXQUEUELEN=1000
I40E_VF_RSS="hfunc toeplitz"
I40E_VF0_MAC=aa:bb:cc:dd:ee:20
I40E_VF1_QUERY_RSS=off
I40E_VF1_RATE=2000
I40E_VF1_ALIAS=vf-one
I40E_VF1_VLAN_QUIRK=1
OUT
    ;;
esac
EOF
  chmod +x "$FIXTURE_BIN_ROOT/udevadm"

  cat > "$FIXTURE_BIN_ROOT/ethtool" <<'EOF'
#!/bin/sh
set -eu

if [ "${1:-}" = "--show-priv-flags" ] && [ "${2:-}" = "testpf0" ]; then
  cat <<'OUT'
Private flags for testpf0:
disable-fw-lldp: off
vf-true-promisc-support: off
OUT
  exit 0
fi

printf 'unexpected ethtool invocation: %s\n' "$*" >&2
exit 1
EOF
  chmod +x "$FIXTURE_BIN_ROOT/ethtool"

  cat > "$FIXTURE_BIN_ROOT/ip" <<'EOF'
#!/bin/sh
set -eu

if [ "${1:-}" = "-d" ] && [ "${2:-}" = "link" ] && [ "${3:-}" = "show" ] && [ "${4:-}" = "testpf0" ]; then
  cat <<'OUT'
5: testpf0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc mq state UP mode DEFAULT group default qlen 1000
    vf 0: MAC aa:bb:cc:dd:ee:10, spoof checking off, link-state auto, trust on
    vf 1: MAC 00:00:00:00:00:00, spoof checking off, link-state auto, trust on
OUT
  exit 0
fi

if [ "${1:-}" = "link" ] && [ "${2:-}" = "show" ]; then
  case "${3:-}" in
    testpf0|testpf0v0|testpf0v1)
      printf '%s\n' "${3}"
      exit 0
      ;;
  esac
  exit 1
fi

printf 'unexpected ip invocation: %s\n' "$*" >&2
exit 1
EOF
  chmod +x "$FIXTURE_BIN_ROOT/ip"
}

run_host_mode() {
  env \
    DRY_RUN=1 \
    "I40E_PF_OFFLOAD=gro off" \
    "I40E_PF_COALESCE=rx-usecs 0 tx-usecs 0" \
    I40E_PF_TXQUEUELEN=1000 \
    I40E_PF_ALIAS=parity-check \
    sh "$SHELL_HELPER" "$IFNAME" >"$TMP_SHELL" 2>&1

  env \
    DRY_RUN=1 \
    "I40E_PF_OFFLOAD=gro off" \
    "I40E_PF_COALESCE=rx-usecs 0 tx-usecs 0" \
    I40E_PF_TXQUEUELEN=1000 \
    I40E_PF_ALIAS=parity-check \
    "$RUST_HELPER" "$IFNAME" >"$TMP_RUST" 2>&1

  compare_case "env-driven PF tuning"

  env \
    DRY_RUN=0 \
    I40E_DISABLE_FW_LLDP=1 \
    I40E_ENABLE_VF_TRUE_PROMISC=1 \
    I40E_DERIVE_VF_MACS=1 \
    sh "$SHELL_HELPER" \
      --dry-run \
      --no-disable-fw-lldp \
      --no-enable-vf-true-promisc \
      --no-derive-vf-macs \
      "$IFNAME" >"$TMP_SHELL" 2>&1

  env \
    DRY_RUN=0 \
    I40E_DISABLE_FW_LLDP=1 \
    I40E_ENABLE_VF_TRUE_PROMISC=1 \
    I40E_DERIVE_VF_MACS=1 \
    "$RUST_HELPER" \
      --dry-run \
      --no-disable-fw-lldp \
      --no-enable-vf-true-promisc \
      --no-derive-vf-macs \
      "$IFNAME" >"$TMP_RUST" 2>&1

  compare_case "CLI overrides beat env"

  vf_count=$(ls -1 "/sys/class/net/$IFNAME"/device/virtfn* 2>/dev/null | wc -l | tr -d ' ')
  printf 'VF coverage on %s: %s virtfn entries\n' "$IFNAME" "$vf_count"

  if [ "$vf_count" = "0" ]; then
    printf '%s\n' 'note: current host exposes no VFs on this PF, so parity coverage is limited to PF-path behavior.'
  fi
}

run_fixture_mode() {
  setup_fixture

  env \
    PATH="$FIXTURE_BIN_ROOT:$PATH" \
    I40E_SYS_CLASS_NET_ROOT="$FIXTURE_SYS_CLASS_NET_ROOT" \
    DRY_RUN=1 \
    sh "$SHELL_HELPER" "$IFNAME" >"$TMP_SHELL" 2>&1

  env \
    PATH="$FIXTURE_BIN_ROOT:$PATH" \
    I40E_SYS_CLASS_NET_ROOT="$FIXTURE_SYS_CLASS_NET_ROOT" \
    DRY_RUN=1 \
    "$RUST_HELPER" "$IFNAME" >"$TMP_RUST" 2>&1

  compare_case "fixture property-driven VF path"

  env \
    PATH="$FIXTURE_BIN_ROOT:$PATH" \
    I40E_SYS_CLASS_NET_ROOT="$FIXTURE_SYS_CLASS_NET_ROOT" \
    DRY_RUN=0 \
    I40E_DISABLE_FW_LLDP=1 \
    I40E_ENABLE_VF_TRUE_PROMISC=1 \
    I40E_DERIVE_VF_MACS=1 \
    sh "$SHELL_HELPER" \
      --dry-run \
      --no-disable-fw-lldp \
      --no-enable-vf-true-promisc \
      --no-derive-vf-macs \
      "$IFNAME" >"$TMP_SHELL" 2>&1

  env \
    PATH="$FIXTURE_BIN_ROOT:$PATH" \
    I40E_SYS_CLASS_NET_ROOT="$FIXTURE_SYS_CLASS_NET_ROOT" \
    DRY_RUN=0 \
    I40E_DISABLE_FW_LLDP=1 \
    I40E_ENABLE_VF_TRUE_PROMISC=1 \
    I40E_DERIVE_VF_MACS=1 \
    "$RUST_HELPER" \
      --dry-run \
      --no-disable-fw-lldp \
      --no-enable-vf-true-promisc \
      --no-derive-vf-macs \
      "$IFNAME" >"$TMP_RUST" 2>&1

  compare_case "fixture CLI overrides beat env"

  printf 'VF coverage on %s: 2 virtfn entries\n' "$IFNAME"
}

printf 'building release binary...\n'
cargo build --release >/dev/null

case "$MODE" in
  fixture) run_fixture_mode ;;
  host) run_host_mode ;;
esac

printf 'dry-run parity matched for shell and Rust\n'