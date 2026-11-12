#!/usr/bin/env bash
# CR-P1-05 (review 2026-11-11 §4.5, 改法第 5 条): the NAT matrix test.
#
# HONESTY — what this is and is NOT:
#   This is a SIMULATION of NAT/firewall behaviors with Linux network
#   namespaces, veth pairs and iptables on ONE kernel (MASQUERADE models
#   port-restricted-cone filtering, MASQUERADE --random-fully models
#   symmetric mappings, static DNAT models full-cone, ip6tables stateful
#   rules model IPv6 routers with inbound filtering). It is repeatable
#   AUTOMATED evidence that catches LOGIC GAPS in the direct-connection
#   path before real devices are touched. It is NOT 真机验证 (real-device
#   verification) — real carrier/hardware NATs differ in ways no iptables
#   emulation covers; that remains the owner's C4 acceptance run
#   (docs/REVIEW_2026-11-12-acceptance.zh-CN.md §7).
#
#   No relay exists anywhere in this topology (ADR-001: relay FORBIDDEN):
#   the only shared host is the services namespace (registry TCP + STUN
#   UDP); file data can structurally never traverse it — a failing case
#   simply fails, it cannot "succeed through a server".
#
# Matrix (per docs/REVIEW_2026-11-11.zh-CN.md §4.5 acceptance):
#   1. port-restricted <-> port-restricted ......... MUST transfer
#   2. port-restricted (alice) <-> full-cone (bob) . MUST transfer
#   3. IPv6 both sides, stateful firewalls ......... MUST transfer (v6 leg)
#   4. symmetric <-> symmetric (--random-fully) .... MUST FAIL cleanly
#      (the E3-class final wording lands in F11; here we assert the clear
#      failure path that exists today: the CLI's bounded no-candidate
#      wording and a non-zero exit — never a hang, never a fake success)
#
# Topology (fresh endpoint/router namespaces per case; the core+services
# namespaces live for the whole run; RAII-style trap cleanup at exit):
#
#   akalice ──(10.10.1.0/24)── akr1 ──(core bridge, 10.10.0.0/24)── akr2 ──(10.10.2.0/24)── akbob
#                                 │                                  │
#                              [NAT A]                           [NAT B]
#                                 └──────────── aksvc ─────────────┘
#                                  (registry TCP + STUN UDP, 10.10.0.10)
#
#   akr1/akr2 are the two "home routers" (independent conntrack and NAT
#   rules per side); akcore hosts the L2 bridge tying r1/r2/svc together.
#   Alice runs `task code-recv` (the RECEIVER), bob runs `task join-send`
#   (the SENDER) — the product CLIs, real registry, real STUN, real QUIC,
#   byte-identical sha256 assertion. Both sides point `--stun` at the
#   in-topology responder so each side's future-QUIC socket learns its
#   mapping (CR-P1-05: the sender verbs carry --stun too).
#
# Skip rules (exit 0 with reasons, like scripts/check_targets.sh): not
# Linux, no root and no passwordless sudo, missing ip/iptables tools, or a
# kernel that refuses namespace/bridge setup. The privileged Linux-only
# path is for CI (ubuntu runners) and rooted Linux hosts; the script
# states that plainly instead of faking results.
#
# Usage:
#   bash scripts/nat_matrix_test.sh [--skip-build] [--size-mb N]
#   (CI: sudo bash scripts/nat_matrix_test.sh — see the nat-matrix job)

set -uo pipefail

TAG="[nat-matrix]"
SIZE_MB=8
SKIP_BUILD=0
PREV_ARG=""
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=1 ;;
    --size-mb) : ;;
    *)
      if [[ "$PREV_ARG" == "--size-mb" ]]; then
        SIZE_MB="$arg"
      fi
      ;;
  esac
  PREV_ARG="$arg"
done

log()  { printf '%s %s\n' "$TAG" "$*"; }
skip() { printf '%s SKIP: %s\n' "$TAG" "$*"; exit 0; }

# ---------------------------------------------------------------------------
# 0. Environment gates (SKIP paths first — the script must be honest about
#    where it can run, and cheap where it cannot).
# ---------------------------------------------------------------------------
case "$(uname -s)" in
  Linux*) : ;;
  *) skip "Linux-only network-namespace test (this host runs $(uname -s)); the privileged Linux path is for CI ubuntu runners and rooted Linux hosts" ;;
esac

for tool in ip iptables ping sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || skip "required tool '$tool' not found in PATH"
done

if [[ "$(id -u)" -ne 0 ]]; then
  if sudo -n true 2>/dev/null; then
    log "not root; re-execing under passwordless sudo (namespaces need CAP_NET_ADMIN)"
    ARGS=()
    [[ "$SKIP_BUILD" -eq 1 ]] && ARGS+=(--skip-build)
    ARGS+=(--size-mb "$SIZE_MB")
    exec sudo -n bash "$0" "${ARGS[@]}"
  fi
  skip "no root and no passwordless sudo (network namespaces need CAP_NET_ADMIN); run with sudo on a Linux host or rely on the CI nat-matrix job"
fi

# ---------------------------------------------------------------------------
# 1. Build the product CLI (the two children are the REAL binaries).
# ---------------------------------------------------------------------------
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLI="$REPO_ROOT/target/debug/ak-cli"
if [[ "$SKIP_BUILD" -ne 1 ]] || [[ ! -x "$CLI" ]]; then
  log "building ak-cli (cargo build -p ak-cli)…"
  (cd "$REPO_ROOT" && cargo build -p ak-cli --locked) || { log "FAIL: cargo build -p ak-cli failed"; exit 1; }
fi
[[ -x "$CLI" ]] || { log "FAIL: $CLI missing after build"; exit 1; }

WORK="$(mktemp -d /tmp/ak-nat-matrix.XXXXXX)"
log "workspace: $WORK"

# ---------------------------------------------------------------------------
# 2. RAII-style teardown: everything the script creates dies with it.
# ---------------------------------------------------------------------------
PIDS_REG="" PIDS_STUN="" PIDS_KIDS=""
NS_A=akalice NS_B=akbob NS_R1=akr1 NS_R2=akr2 NS_CORE=akcore NS_SVC=aksvc
ALL_NS=("$NS_A" "$NS_B" "$NS_R1" "$NS_R2" "$NS_CORE" "$NS_SVC")
cleanup() {
  for pid in $PIDS_KIDS $PIDS_STUN $PIDS_REG; do
    kill "$pid" 2>/dev/null || true
  done
  for ns in "${ALL_NS[@]}"; do
    ip netns del "$ns" 2>/dev/null || true
  done
  rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# 3. Topology builders.
# ---------------------------------------------------------------------------
# Interfaces (veth names must be <= 15 chars):
#   alice: a-to-r1 (10.10.1.2/24, v6 2001:77:1::2/64)
#   r1:    r1-to-a (10.10.1.1/24, v6 2001:77:1::1/64),
#          r1-to-c (10.10.0.1/24, v6 2001:77:ff::1/64)
#   core:  bridge akbr0 with c-to-r1, c-to-r2, c-to-svc (L2 only)
#   r2:    r2-to-c (10.10.0.2/24, v6 2001:77:ff::2/64),
#          r2-to-b (10.10.2.1/24, v6 2001:77:2::1/64)
#   bob:   b-to-r2 (10.10.2.2/24, v6 2001:77:2::2/64)
#   svc:   svc-to-c (10.10.0.10/24)

make_ns() { ip netns add "$1" && ip -n "$1" link set lo up; }

link_ns() { # link_ns <ns-a> <if-a> <ns-b> <if-b>
  ip link add "$2" type veth peer name "$4"
  ip link set "$2" netns "$1"
  ip link set "$4" netns "$3"
}

build_core() {
  # The core bridge + the services namespace live for the whole run.
  make_ns "$NS_CORE"
  make_ns "$NS_SVC"
  ip -n "$NS_CORE" link add akbr0 type bridge
  ip -n "$NS_CORE" link set akbr0 up
  link_ns "$NS_SVC" svc-to-c "$NS_CORE" c-to-svc
  ip -n "$NS_CORE" link set c-to-svc up
  ip -n "$NS_CORE" link set c-to-svc master akbr0
  ip -n "$NS_SVC" addr add 10.10.0.10/24 dev svc-to-c
  ip -n "$NS_SVC" link set svc-to-c up
}

build_case_topology() {
  # Fresh endpoints and routers per case; they re-attach to the persistent
  # core bridge with fresh veths.
  for ns in "$NS_A" "$NS_B" "$NS_R1" "$NS_R2"; do
    ip netns del "$ns" 2>/dev/null || true
  done
  for ns in "$NS_A" "$NS_B" "$NS_R1" "$NS_R2"; do
    make_ns "$ns" || return 1
  done
  link_ns "$NS_R1" r1-to-c "$NS_CORE" c-to-r1
  link_ns "$NS_R2" r2-to-c "$NS_CORE" c-to-r2
  ip -n "$NS_CORE" link set c-to-r1 up
  ip -n "$NS_CORE" link set c-to-r2 master akbr0
  ip -n "$NS_CORE" link set c-to-r2 up 2>/dev/null || true
  ip -n "$NS_CORE" link set c-to-r1 master akbr0 2>/dev/null || true
  # Alice <-> r1.
  link_ns "$NS_A" a-to-r1 "$NS_R1" r1-to-a
  ip -n "$NS_A" addr add 10.10.1.2/24 dev a-to-r1
  ip -n "$NS_A" -6 addr add 2001:77:1::2/64 dev a-to-r1
  ip -n "$NS_A" link set a-to-r1 up
  ip -n "$NS_A" route add default via 10.10.1.1
  ip -n "$NS_A" -6 route add default via 2001:77:1::1
  ip -n "$NS_R1" addr add 10.10.1.1/24 dev r1-to-a
  ip -n "$NS_R1" -6 addr add 2001:77:1::1/64 dev r1-to-a
  ip -n "$NS_R1" addr add 10.10.0.1/24 dev r1-to-c
  ip -n "$NS_R1" -6 addr add 2001:77:ff::1/64 dev r1-to-c
  ip -n "$NS_R1" link set r1-to-a up
  ip -n "$NS_R1" link set r1-to-c up
  # Bob <-> r2.
  link_ns "$NS_B" b-to-r2 "$NS_R2" r2-to-b
  ip -n "$NS_B" addr add 10.10.2.2/24 dev b-to-r2
  ip -n "$NS_B" -6 addr add 2001:77:2::2/64 dev b-to-r2
  ip -n "$NS_B" link set b-to-r2 up
  ip -n "$NS_B" route add default via 10.10.2.1
  ip -n "$NS_B" -6 route add default via 2001:77:2::1
  ip -n "$NS_R2" addr add 10.10.2.1/24 dev r2-to-b
  ip -n "$NS_R2" -6 addr add 2001:77:2::1/64 dev r2-to-b
  ip -n "$NS_R2" addr add 10.10.0.2/24 dev r2-to-c
  ip -n "$NS_R2" -6 addr add 2001:77:ff::2/64 dev r2-to-c
  ip -n "$NS_R2" link set r2-to-b up
  ip -n "$NS_R2" link set r2-to-c up
  # Routing: each router knows the peer LAN via the core; endpoints default
  # to their router; the services host reaches both routers directly.
  ip -n "$NS_R1" route add 10.10.2.0/24 via 10.10.0.2
  ip -n "$NS_R2" route add 10.10.1.0/24 via 10.10.0.1
  ip -n "$NS_R1" -6 route add 2001:77:2::/64 via 2001:77:ff::2
  ip -n "$NS_R2" -6 route add 2001:77:1::/64 via 2001:77:ff::1
  for ns in "$NS_R1" "$NS_R2"; do
    ip netns exec "$ns" sysctl -qw net.ipv4.ip_forward=1
    ip netns exec "$ns" sysctl -qw net.ipv6.conf.all.forwarding=1
  done
  # Baseline connectivity smoke: both endpoints reach the services host.
  ip netns exec "$NS_A" ping -c1 -W2 10.10.0.10 >/dev/null 2>&1 \
    || { log "FAIL: alice cannot reach the services host (base topology broken)"; return 1; }
  ip netns exec "$NS_B" ping -c1 -W2 10.10.0.10 >/dev/null 2>&1 \
    || { log "FAIL: bob cannot reach the services host (base topology broken)"; return 1; }
}

# nat_rules <router-ns> <lan-iface> <core-iface> <lan-subnet> <mode>
#   mode: pcone | fullcone | symmetric | control-only
# The stateful base (outbound NEW allowed, inbound only ESTABLISHED) IS the
# port-restricted filtering; the modes add the mapping flavor.
nat_rules() {
  local ns="$1" lan="$2" core="$3" subnet="$4" mode="$5"
  ip netns exec "$ns" iptables -P FORWARD DROP
  ip netns exec "$ns" iptables -F FORWARD
  ip netns exec "$ns" iptables -F -t nat
  ip netns exec "$ns" iptables -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  case "$mode" in
    pcone)
      ip netns exec "$ns" iptables -A FORWARD -i "$lan" -j ACCEPT
      ip netns exec "$ns" iptables -t nat -A POSTROUTING -s "$subnet" -o "$core" -j MASQUERADE
      ;;
    fullcone)
      # 1:1 static DNAT (port-preserving) + unconditional inbound UDP to the
      # LAN host: any outsider may send to the published endpoint = full cone.
      local host_ip
      host_ip="${subnet%0/24}2"
      ip netns exec "$ns" iptables -A FORWARD -i "$lan" -j ACCEPT
      ip netns exec "$ns" iptables -t nat -A POSTROUTING -s "$subnet" -o "$core" -j MASQUERADE
      ip netns exec "$ns" iptables -t nat -A PREROUTING -i "$core" -p udp -j DNAT --to-destination "$host_ip"
      ip netns exec "$ns" iptables -A FORWARD -i "$core" -p udp -d "$host_ip" -j ACCEPT
      ;;
    symmetric)
      ip netns exec "$ns" iptables -A FORWARD -i "$lan" -j ACCEPT
      ip netns exec "$ns" iptables -t nat -A POSTROUTING -s "$subnet" -o "$core" -j MASQUERADE --random-fully
      ;;
    control-only)
      # v6 case: the v4 data plane must die (peer dials are not to the
      # services host), while the control plane (registry/STUN, always
      # initiated outbound toward 10.10.0.10) stays alive.
      ip netns exec "$ns" iptables -A FORWARD -i "$lan" -d 10.10.0.10 -j ACCEPT
      ;;
  esac
}

# v6_firewall <router-ns> <lan-iface>: stateful inbound filtering for the
# IPv6 data plane (outbound NEW allowed; inbound only ESTABLISHED — the
# receiver's punch creates the state that admits the sender's Initial).
v6_firewall() {
  local ns="$1" lan="$2"
  ip netns exec "$ns" ip6tables -P FORWARD DROP
  ip netns exec "$ns" ip6tables -F FORWARD
  ip netns exec "$ns" ip6tables -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  ip netns exec "$ns" ip6tables -A FORWARD -i "$lan" -j ACCEPT
}

# ---------------------------------------------------------------------------
# 4. Services: the real registry + the real standalone STUN responder
#    (F3's `ak-cli stun-server`), both inside the services namespace.
# ---------------------------------------------------------------------------
REG_PORT=17300
STUN_PORT=17101
start_services() {
  mkdir -p "$WORK/svc-id"
  ip netns exec "$NS_SVC" "$CLI" registry-server \
    --port "$REG_PORT" --db-path "$WORK/registry.db" --id-dir "$WORK/svc-id" \
    >"$WORK/registry.log" 2>&1 &
  PIDS_REG=$!
  ip netns exec "$NS_SVC" "$CLI" stun-server --port "$STUN_PORT" \
    >"$WORK/stun.log" 2>&1 &
  PIDS_STUN=$!
  local waited=0 line
  while [[ $waited -lt 300 ]]; do
    grep -q "registry id (pin with --registry-fp): " "$WORK/registry.log" 2>/dev/null && break
    sleep 0.2; waited=$((waited + 1))
  done
  line="$(grep -m1 "registry id (pin with --registry-fp): " "$WORK/registry.log" || true)"
  REG_FP="$(printf '%s' "$line" | awk '{print $NF}')"
  [[ "${#REG_FP}" -eq 64 ]] || { log "FAIL: registry did not come up (log: $(tail -3 "$WORK/registry.log" 2>/dev/null))"; return 1; }
  waited=0
  while [[ $waited -lt 150 ]]; do
    grep -q "stun responder" "$WORK/stun.log" 2>/dev/null && break
    sleep 0.2; waited=$((waited + 1))
  done
  grep -q "stun responder" "$WORK/stun.log" 2>/dev/null \
    || { log "FAIL: stun-server did not come up (log: $(tail -3 "$WORK/stun.log" 2>/dev/null))"; return 1; }
  log "services up: registry 10.10.0.10:$REG_PORT (fp ${REG_FP:0:8}…), stun 10.10.0.10:$STUN_PORT"
}

# ---------------------------------------------------------------------------
# 5. One transfer case. run_case <case-name> <expect: pass|fail|pass-v6>
#    alice = `task code-recv` (receiver), bob = `task join-send` (sender).
# ---------------------------------------------------------------------------
wait_for_text() { # wait_for_text <file> <needle> <timeout-secs> -> 0/1
  local file="$1" needle="$2" budget="$3" waited=0
  while [[ $waited -lt $((budget * 10)) ]]; do
    grep -q "$needle" "$file" 2>/dev/null && return 0
    sleep 0.1; waited=$((waited + 1))
  done
  return 1
}

run_case() {
  local case_name="$1" expect="$2" case_dir="$WORK/$case_name"
  mkdir -p "$case_dir/id-alice" "$case_dir/id-bob" "$case_dir/out" "$case_dir/src"
  dd if=/dev/urandom of="$case_dir/src/payload.bin" bs=1M count="$SIZE_MB" status=none

  # The receiver: consent auto-fed with a fresh 'y' stream (the CLI
  # discards stale input; `yes` keeps supplying fresh lines).
  yes | ip netns exec "$NS_A" "$CLI" --id-dir "$case_dir/id-alice" task code-recv \
      --out "$case_dir/out" \
      --server "10.10.0.10:$REG_PORT" --registry-fp "$REG_FP" \
      --stun "10.10.0.10:$STUN_PORT" \
      >"$case_dir/recv.log" 2>&1 &
  local recv_pid=$!
  PIDS_KIDS="$PIDS_KIDS $recv_pid"
  wait_for_text "$case_dir/recv.log" "AKI1-" 120 \
    || { log "FAIL [$case_name]: receiver never printed a code"; return 1; }
  local code
  code="$(grep -o 'AKI1-[0-9A-Z-]*' "$case_dir/recv.log" | head -1)"
  [[ -n "$code" ]] || { log "FAIL [$case_name]: no parsable code"; return 1; }

  # The sender.
  ip netns exec "$NS_B" "$CLI" --id-dir "$case_dir/id-bob" task join-send \
      "$code" "$case_dir/src/payload.bin" \
      --server "10.10.0.10:$REG_PORT" --registry-fp "$REG_FP" \
      --stun "10.10.0.10:$STUN_PORT" \
      >"$case_dir/send.log" 2>&1 &
  local send_pid=$!
  PIDS_KIDS="$PIDS_KIDS $send_pid"

  # Bounded wait for the sender to exit (fail cases legitimately take ~2
  # minutes of bounded retries; pass cases seconds; a hang is never OK).
  local waited=0
  while kill -0 "$send_pid" 2>/dev/null && [[ $waited -lt 2400 ]]; do
    sleep 0.1; waited=$((waited + 1))
  done
  if kill -0 "$send_pid" 2>/dev/null; then
    kill "$send_pid" 2>/dev/null
    log "FAIL [$case_name]: sender did not finish within 240s (hang — never acceptable)"
    return 1
  fi
  wait "$send_pid"
  local send_rc=$?
  # Let the receiver finish its own teardown (bounded).
  local rwait=0
  while kill -0 "$recv_pid" 2>/dev/null && [[ $rwait -lt 150 ]]; do
    sleep 0.2; rwait=$((rwait + 1))
  done
  kill "$recv_pid" 2>/dev/null

  local send_log="$case_dir/send.log" recv_log="$case_dir/recv.log"
  if [[ "$expect" == "pass" || "$expect" == "pass-v6" ]]; then
    grep -q "\[task\] done:" "$send_log" \
      || { log "FAIL [$case_name]: sender did not complete (rc=$send_rc)"; tail -5 "$send_log" | sed 's/^/    /'; return 1; }
    grep -q "\[task\] received" "$recv_log" \
      || { log "FAIL [$case_name]: receiver did not complete"; tail -5 "$recv_log" | sed 's/^/    /'; return 1; }
    grep -q "\[task\] connected via " "$send_log" \
      || { log "FAIL [$case_name]: no connected-via line"; tail -5 "$send_log" | sed 's/^/    /'; return 1; }
    local via
    via="$(grep -m1 '\[task\] connected via ' "$send_log" | awk '{print $NF}')"
    if [[ "$expect" == "pass-v6" ]]; then
      [[ "$via" == *:* ]] \
        || { log "FAIL [$case_name]: expected the IPv6 leg, carried by '$via'"; return 1; }
    fi
    local sum_src sum_dst
    sum_src="$(sha256sum "$case_dir/src/payload.bin" | awk '{print $1}')"
    sum_dst="$(sha256sum "$case_dir/out/payload.bin" 2>/dev/null | awk '{print $1}')"
    [[ -n "$sum_dst" && "$sum_src" == "$sum_dst" ]] \
      || { log "FAIL [$case_name]: payload sha256 mismatch (src=$sum_src dst=$sum_dst)"; return 1; }
    log "PASS [$case_name]: direct transfer, ${SIZE_MB}MiB byte-identical via $via"
  else
    [[ $send_rc -ne 0 ]] \
      || { log "FAIL [$case_name]: symmetric<->symmetric must NOT succeed (E3 territory)"; grep -m1 "done:" "$send_log"; return 1; }
    grep -q "no candidate accepted the connection" "$send_log" \
      || { log "FAIL [$case_name]: the failure must be the bounded no-candidate wording (rc=$send_rc)"; tail -5 "$send_log" | sed 's/^/    /'; return 1; }
    if [[ -e "$case_dir/out/payload.bin" ]]; then
      log "FAIL [$case_name]: payload landed despite the failed transfer — impossible without a relay (P0-class)"
      return 1
    fi
    log "PASS [$case_name]: symmetric<->symmetric failed cleanly (bounded wording, no hang, no data moved)"
  fi
  return 0
}

# ---------------------------------------------------------------------------
# 6. The matrix.
# ---------------------------------------------------------------------------
FAILURES=0
case_pass() { # case_pass <name> <desc> <r1-mode> <r2-mode> <v6:0|1> <expect>
  local name="$1" desc="$2" r1="$3" r2="$4" use_v6="$5" expect="$6"
  log "---- case $name: $desc"
  build_case_topology || { log "FAIL [$name]: topology setup failed"; FAILURES=$((FAILURES + 1)); return; }
  nat_rules "$NS_R1" r1-to-a r1-to-c 10.10.1.0/24 "$r1"
  nat_rules "$NS_R2" r2-to-b r2-to-c 10.10.2.0/24 "$r2"
  if [[ "$use_v6" -eq 1 ]]; then
    v6_firewall "$NS_R1" r1-to-a
    v6_firewall "$NS_R2" r2-to-b
  fi
  run_case "$name" "$expect" || FAILURES=$((FAILURES + 1))
}

build_core || { log "FAIL: core topology setup failed (kernel refusing namespaces/bridges?)"; exit 1; }
start_services || exit 1

case_pass "pcone-pcone"    "port-restricted <-> port-restricted (MASQUERADE + stateful both)" pcone pcone 0 pass
case_pass "pcone-fullcone" "port-restricted (alice) <-> full-cone DNAT (bob)"                 pcone fullcone 0 pass
if command -v ip6tables >/dev/null 2>&1; then
  case_pass "v6-stateful"  "IPv6 both sides behind stateful firewalls (v4 control-plane only)" control-only control-only 1 pass-v6
else
  log "SKIP case v6-stateful: ip6tables not available on this host"
fi
case_pass "symm-symm"      "symmetric <-> symmetric (--random-fully; EXPECTED FAILURE)"        symmetric symmetric 0 fail

log "======================================"
if [[ $FAILURES -eq 0 ]]; then
  log "RESULT: all runnable matrix cases PASS"
  exit 0
fi
log "RESULT: $FAILURES matrix case(s) FAILED"
exit 1
