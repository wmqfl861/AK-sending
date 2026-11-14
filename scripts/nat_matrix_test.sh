#!/usr/bin/env bash
# CR-P1-05 (review 2026-11-11 §4.5, 改法第 5 条) + CR-P2-14 (batchF review
# 2026-11-12: evidence-fidelity hardening): the NAT matrix test.
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
# Matrix (CR-P2-14 semantics):
#   1. port-restricted <-> port-restricted ... strict xfail until CR-P1-06
#      is fixed (IPv4 tuple-conflict: the receiver's punch arrives at the
#      sender's NAT first and the sender's SNAT then changes its external
#      port; batchF review §4.3). Unexpected pass = RED ("flip to
#      pass-v4"). After CR-P1-06: expect pass-v4.
#   2. port-restricted (alice) <-> full-cone (bob) ... pass-v4: the
#      transfer must ride the peer router's public address (203.0.113.1).
#   3. IPv6 both sides, stateful firewalls ... pass-v6 (v4 data plane
#      restricted to the services host; endpoints v6-only candidates).
#   4. symmetric <-> symmetric (--random-fully) ... MUST FAIL cleanly
#      (bounded wording, no hang, zero payload; in this closed simulation
#      there is no v4 path, so a "success" means the fixture lies).
#   5. late send, >=5 s tier (CR-P1-07, review 2026-11-13 §4.4 改法 4):
#      pcone<->pcone, the sender's send start delayed past the receiver's
#      4 s punch window (the GUI's pair-then-pick-files timing). The
#      pairing-time opener + mapping keepalive must carry the connection.
#   6. late send, >=40 s tier: same, past the 30 s unreplied-conntrack
#      timeout — only the keepalive's refreshes keep both mappings alive.
#   7. receiver without low TTL (CR-P1-08): pcone<->pcone with the
#      receiver's low-TTL punch phase disabled (AK_EXP_NO_TTL — the
#      pre-H1 Windows shape). Must still pass: the sender's pairing-time
#      opener rides a LOW TTL (never reaches the receiver's NAT) and the
#      receiver delays its whole punch past that window.
#   8. decline then retry >=40 s (CR-P3-15): pcone<->pcone, the receiver
#      declines the FIRST offer (decline-first stdin feeder), the sender
#      retries after >=40 s (AK_EXP_SEND_RETRY_MS — the GUI's "declined,
#      user tries again later" timing). During the wait the mapping
#      keepalive resumes within the pairing's remaining idle budget; the
#      retry must ride the refreshed mappings (pass-v4).
#
#   IPv4 cases do not configure endpoint IPv6 addresses AND set
#   ip6tables FORWARD DROP on both routers (CR-P2-14: the batchF runs
#   let the product bypass the NATs over IPv6 and counted it as v4
#   success). Every case runs a FIXTURE SELF-CHECK before the transfer:
#   one socket queries two STUN destinations; pcone/fullcone must show
#   the SAME external port, symmetric must show DIFFERENT ones — else
#   the case is judged "fixture invalid", not a product result.
#
# Topology (fresh endpoint/router namespaces per case; core/inet/services
# live for the whole run; RAII-style trap cleanup at exit):
#
#   CR-P3-13 (H3, review 2026-11-13 §5.3): the CORE and INET segments use
#   RFC 5737 documentation ranges (203.0.113.0/24 = TEST-NET-3,
#   198.51.100.0/24 = TEST-NET-2) so `prefers_lan` classifies them PUBLIC
#   exactly as it would a real deployment — with the old RFC1918
#   10.10.0.0/24 the reflexive address was "LAN", and the production
#   "reflexive first" / "short budget for the first public candidate"
#   paths never ran in the matrix (the matrix only proved the
#   observed-address path). The home LANs (10.10.1.0/24, 10.10.2.0/24)
#   stay RFC1918 on purpose — they model the private side of the NAT.
#
#   akalice ─(10.10.1.0/24)─ akr1 ─┐                        ┌─(198.51.100.0/24)─ akr2 ─(10.10.2.0/24)─ akbob
#                             [NAT] │      akcore (L2)       │      [NAT]
#                                   ├─ 203.0.113.0/24 bridge ──┤ i-to-r2 198.51.100.254
#                                 r1-to-c 203.0.113.1          │
#                                                            │
#                          aksvc 203.0.113.10 (registry+STUN)  │
#                          c-to-i 203.0.113.254 ───────────────┘ (akinet, the
#                                                                   "public internet"
#                                                                   routed hop — no
#                                                                   NAT, enables
#                                                                   mid-path TTL
#                                                                   expiry for
#                                                                   CR-P1-06's
#                                                                   low-TTL punch)
#   v6 (v6 case): 2001:77:1::/64 alice-r1, 2001:77:ff::/64 bridge,
#                 2001:77:ee::/64 inet-r2, 2001:77:2::/64 r2-bob.
#
#   akr1/akr2 are the two "home routers" (independent conntrack and NAT
#   rules per side); akinet is a plain routed hop between them (the
#   "public internet" — forwarding only, no NAT, so a low-TTL packet can
#   die mid-path as it would on the real internet); aksvc hosts registry
#   TCP + two STUN UDP responders (the second exists for the fixture
#   self-check's two-destination probe). Alice runs `task code-recv`
#   (the RECEIVER), bob runs `task join-send` (the SENDER) — the product
#   CLIs, real registry, real STUN, real QUIC, byte-identical sha256
#   assertion. Both sides point `--stun` at the in-topology responder.
#
# Skip rules (exit 0 with reasons, like scripts/check_targets.sh): not
# Linux, no root and no passwordless sudo, missing ip/iptables/ping/
# sha256sum/python3 tools, or a kernel that refuses namespace/bridge
# setup. The privileged Linux-only path is for CI (ubuntu runners) and
# rooted Linux hosts; the script states that plainly instead of faking
# results.
#
# Usage:
#   bash scripts/nat_matrix_test.sh [--skip-build] [--size-mb N]
#   (CI: sudo bash scripts/nat_matrix_test.sh --skip-build — see the
#    nat-matrix job; the runner user builds, root orchestrates)

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

# Run a critical setup command; on failure log the stage and return 1 from
# the CALLER (use as `must "stage" cmd args...` inside functions that
# `return 1` on error). G0 hardening: `set -uo pipefail` does not stop
# plain failures when functions run in `||` contexts, so critical ip/
# sysctl/iptables calls are checked explicitly (judgment-layer finding C).
must() {
  local stage="$1"; shift
  if ! "$@"; then
    log "FAIL: setup step failed: $stage ($*)"
    return 1
  fi
}
skip() { printf '%s SKIP: %s\n' "$TAG" "$*"; exit 0; }

# ---------------------------------------------------------------------------
# 0. Environment gates (SKIP paths first — the script must be honest about
#    where it can run, and cheap where it cannot).
# ---------------------------------------------------------------------------
case "$(uname -s)" in
  Linux*) : ;;
  *) skip "Linux-only network-namespace test (this host runs $(uname -s)); the privileged Linux path is for CI ubuntu runners and rooted Linux hosts" ;;
esac

for tool in ip iptables ping sha256sum python3; do
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
# CR-P3-16 (review 2026-11-13-batchH §4): the matrix drives the hidden
# AK_EXP_* knobs, which compile ONLY in exp-knobs builds — arm the
# feature here so the late-send / no-low-TTL / decline-retry cases have
# their switches. The product default (no feature) keeps the knobs absent.
if [[ "$SKIP_BUILD" -ne 1 ]] || [[ ! -x "$CLI" ]]; then
  log "building ak-cli (cargo build -p ak-cli --features exp-knobs)…"
  (cd "$REPO_ROOT" && cargo build -p ak-cli --locked --features exp-knobs) || { log "FAIL: cargo build -p ak-cli failed"; exit 1; }
fi
[[ -x "$CLI" ]] || { log "FAIL: $CLI missing after build"; exit 1; }

WORK="$(mktemp -d /tmp/ak-nat-matrix.XXXXXX)"
log "workspace: $WORK"

# ---------------------------------------------------------------------------
# 2. RAII-style teardown: everything the script creates dies with it.
# ---------------------------------------------------------------------------
PIDS_REG="" PIDS_STUN="" PIDS_KIDS=""
NS_A=akalice NS_B=akbob NS_R1=akr1 NS_R2=akr2 NS_CORE=akcore NS_SVC=aksvc
NS_INET=akinet
ALL_NS=("$NS_A" "$NS_B" "$NS_R1" "$NS_R2" "$NS_CORE" "$NS_SVC" "$NS_INET")
OWNED_NS=()
CLEANED_UP=0

# Wait only after a tracked process has exited. In particular, never wait on
# a pipeline whose untracked stdin feeder can keep the job alive indefinitely.
stop_pids() {
  local pid attempt remaining=""
  for pid in "$@"; do
    [[ "$pid" =~ ^[1-9][0-9]*$ ]] || continue
    kill -TERM "$pid" 2>/dev/null || true
  done
  for ((attempt=0; attempt<20; attempt++)); do
    remaining=""
    for pid in "$@"; do
      [[ "$pid" =~ ^[1-9][0-9]*$ ]] || continue
      kill -0 "$pid" 2>/dev/null && remaining="$remaining $pid"
    done
    [[ -n "$remaining" ]] || break
    sleep 0.1
  done
  for pid in $remaining; do kill -KILL "$pid" 2>/dev/null || true; done
  for ((attempt=0; attempt<20; attempt++)); do
    remaining=""
    for pid in "$@"; do
      [[ "$pid" =~ ^[1-9][0-9]*$ ]] || continue
      kill -0 "$pid" 2>/dev/null && remaining="$remaining $pid"
    done
    [[ -n "$remaining" ]] || break
    sleep 0.1
  done
  for pid in "$@"; do
    [[ "$pid" =~ ^[1-9][0-9]*$ ]] || continue
    if ! kill -0 "$pid" 2>/dev/null; then wait "$pid" 2>/dev/null || true; fi
  done
  [[ -z "$remaining" ]]
}

# Auxiliary workers launch external commands asynchronously so TERM interrupts
# wait immediately. Only their own child jobs are reaped, never sibling jobs.
finish_worker() {
  local pid
  trap - EXIT
  trap '' INT TERM
  local children
  children="$(jobs -pr)"
  for pid in $children; do kill -KILL "$pid" 2>/dev/null || true; done
  for pid in $children; do wait "$pid" 2>/dev/null || true; done
}

worker_command() {
  "$@" &
  wait "$!"
}

stop_case_children() {
  stop_pids $PIDS_KIDS || { log "FAIL: case child cleanup exceeded its bound"; return 1; }
  PIDS_KIDS=""
}

delete_owned_ns() {
  local ns="$1" owned found=0 pids
  local keep=()
  for owned in "${OWNED_NS[@]}"; do
    [[ "$owned" != "$ns" ]] || found=1
  done
  [[ "$found" -eq 1 ]] || return 0
  pids="$(ip netns pids "$ns")" || return 1
  stop_pids $pids || return 1
  pids="$(ip netns pids "$ns")" || return 1
  [[ -z "$pids" ]] || return 1
  ip netns del "$ns" || return 1
  for owned in "${OWNED_NS[@]}"; do
    [[ "$owned" == "$ns" ]] || keep+=("$owned")
  done
  OWNED_NS=("${keep[@]}")
}

cleanup() {
  [[ "$CLEANED_UP" -eq 0 ]] || return 0
  CLEANED_UP=1
  local ns failed=0
  stop_case_children || failed=1
  stop_pids $PIDS_STUN $PIDS_REG || failed=1
  PIDS_STUN="" PIDS_REG=""
  for ns in "${OWNED_NS[@]}"; do
    delete_owned_ns "$ns" || { log "FAIL: namespace cleanup failed for $ns"; failed=1; }
  done
  rm -rf "$WORK" 2>/dev/null || failed=1
  return "$failed"
}
trap 'cleanup_rc=$?; trap - EXIT; trap "" INT TERM; cleanup || cleanup_rc=1; exit "$cleanup_rc"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ---------------------------------------------------------------------------
# 3. Topology builders.
# ---------------------------------------------------------------------------
# Interfaces (veth names must be <= 15 chars):
#   alice: a-to-r1 (10.10.1.2/24; v6 only in the v6 case)
#   r1:    r1-to-a (10.10.1.1/24), r1-to-c (203.0.113.1/24)
#   core:  bridge akbr0 with c-to-r1, c-to-svc, c-to-i (L2 only)
#   svc:   svc-to-c (203.0.113.10/24)
#   inet:  i-to-c (203.0.113.254/24, v6 2001:77:ff::254/64),
#          i-to-r2 (198.51.100.254/24, v6 2001:77:ee::1/64)
#   r2:    r2-to-i (198.51.100.2/24), r2-to-b (10.10.2.1/24)
#   bob:   b-to-r2 (10.10.2.2/24)

# Route/interface dump for every namespace — printed on topology smoke
# failures so a CI log is self-diagnosing instead of a blind 40-minute
# iteration cycle.
dump_routes() {
  for ns in "${ALL_NS[@]}"; do
    log "--- routes in $ns:"
    ip -n "$ns" route show 2>&1 | while read -r line; do log "    $line"; done
    ip -n "$ns" -6 route show 2>&1 | while read -r line; do log "    $line"; done
    log "--- links in $ns:"
    ip -n "$ns" -br link 2>&1 | while read -r line; do log "    $line"; done
  done
}

# conntrack dump of both NAT routers — printed on ANY case failure
# (CR-P2-14 point 4: the batchF root-cause evidence was exactly this
# table; make it automatic).
dump_conntrack() {
  local ns
  for ns in "$NS_R1" "$NS_R2"; do
    log "--- conntrack (udp) in $ns:"
    ip netns exec "$ns" sh -c \
      'command -v conntrack >/dev/null 2>&1 && conntrack -L -p udp 2>/dev/null || grep -a "udp" /proc/net/nf_conntrack 2>/dev/null' \
      | while read -r line; do log "    $line"; done
  done
}

make_ns() {
  ip netns add "$1" || return 1
  OWNED_NS+=("$1")
  ip -n "$1" link set lo up
}

link_ns() { # link_ns <ns-a> <if-a> <ns-b> <if-b>
  ip link add "$2" type veth peer name "$4"
  ip link set "$2" netns "$1"
  ip link set "$4" netns "$3"
}

# CR-P2-14 point 2: deleting a netns destroys its veths and their peers
# ASYNCHRONOUSLY; recreating same-name interfaces races the kernel's
# cleanup ("An interface with the same name exists in the target netns",
# CI run 43188955077). Poll until the stale names are really gone.
wait_iface_gone() { # wait_iface_gone <ns> <iface> [budget-deciseconds]
  local ns="$1" iface="$2" budget="${3:-50}" waited=0
  while ip -n "$ns" link show dev "$iface" >/dev/null 2>&1; do
    [[ $waited -ge $budget ]] && return 1
    sleep 0.1; waited=$((waited + 1))
  done
  return 0
}

build_core() {
  # The core bridge, the "public internet" routed hop and the services
  # namespace live for the whole run.
  make_ns "$NS_CORE" || return 1
  make_ns "$NS_SVC" || return 1
  make_ns "$NS_INET" || return 1
  ip -n "$NS_CORE" link add akbr0 type bridge
  ip -n "$NS_CORE" link set akbr0 up
  link_ns "$NS_SVC" svc-to-c "$NS_CORE" c-to-svc
  ip -n "$NS_CORE" link set c-to-svc up
  ip -n "$NS_CORE" link set c-to-svc master akbr0
  ip -n "$NS_SVC" addr add 203.0.113.10/24 dev svc-to-c
  ip -n "$NS_SVC" link set svc-to-c up
  link_ns "$NS_INET" i-to-c "$NS_CORE" c-to-i
  ip -n "$NS_CORE" link set c-to-i up
  ip -n "$NS_CORE" link set c-to-i master akbr0
  ip -n "$NS_INET" addr add 203.0.113.254/24 dev i-to-c
  ip -n "$NS_INET" -6 addr add 2001:77:ff::254/64 dev i-to-c
  ip -n "$NS_INET" link set i-to-c up
  # akinet is a plain ROUTED hop (the "public internet"): forwarding on,
  # no NAT, default-accept filtering — its whole purpose is to give
  # low-TTL packets a mid-path place to expire (CR-P1-06 改法 5).
  ip netns exec "$NS_INET" sysctl -qw net.ipv4.ip_forward=1
  ip netns exec "$NS_INET" sysctl -qw net.ipv6.conf.all.forwarding=1
  ip netns exec "$NS_INET" sysctl -qw net.ipv4.conf.all.rp_filter=0
}

# build_case_topology <use_v6:0|1>
#  Fresh endpoints and routers per case. For v4 cases (use_v6=0) the
#  endpoints get NO IPv6 addresses at all (CR-P2-14 point 1: with v6
#  configured, the product simply talked v6 past the NATs under test);
#  the routers additionally get ip6tables FORWARD DROP in case_pass.
build_case_topology() {
  local use_v6="$1"
  stop_case_children || return 1
  for ns in "$NS_A" "$NS_B" "$NS_R1" "$NS_R2"; do
    delete_owned_ns "$ns" || return 1
  done
  # Wait for the asynchronous veth teardown (see wait_iface_gone): the
  # core/inet-side peers of the deleted routers' links must be gone
  # before same-name links are recreated.
  wait_iface_gone "$NS_CORE" c-to-r1 || { log "FAIL: stale c-to-r1 never vanished"; return 1; }
  wait_iface_gone "$NS_CORE" c-to-r2 || { log "FAIL: stale c-to-r2 never vanished"; return 1; }
  wait_iface_gone "$NS_INET" i-to-r2 || { log "FAIL: stale i-to-r2 never vanished"; return 1; }
  for ns in "$NS_A" "$NS_B" "$NS_R1" "$NS_R2"; do
    make_ns "$ns" || return 1
  done
  # r1 attaches to the core bridge; r2 attaches to the "public internet".
  link_ns "$NS_R1" r1-to-c "$NS_CORE" c-to-r1
  ip -n "$NS_CORE" link set c-to-r1 up
  ip -n "$NS_CORE" link set c-to-r1 master akbr0
  link_ns "$NS_R2" r2-to-i "$NS_INET" i-to-r2
  # G0 fix (run 43253690898 root cause): the IPv4 gateway address on the
  # persistent inet side was NEVER configured (only the v6 case added an
  # IPv6 address) — every `via 198.51.100.2` route below died with "Nexthop
  # has invalid gateway" and all four cases stopped at the pre-NAT smoke.
  ip -n "$NS_INET" addr add 198.51.100.254/24 dev i-to-r2
  ip -n "$NS_INET" link set i-to-r2 up
  # Alice <-> r1.
  link_ns "$NS_A" a-to-r1 "$NS_R1" r1-to-a
  ip -n "$NS_A" addr add 10.10.1.2/24 dev a-to-r1
  ip -n "$NS_A" link set a-to-r1 up
  ip -n "$NS_A" route add default via 10.10.1.1
  ip -n "$NS_R1" addr add 10.10.1.1/24 dev r1-to-a
  ip -n "$NS_R1" addr add 203.0.113.1/24 dev r1-to-c
  ip -n "$NS_R1" link set r1-to-a up
  ip -n "$NS_R1" link set r1-to-c up
  # Bob <-> r2.
  link_ns "$NS_B" b-to-r2 "$NS_R2" r2-to-b
  ip -n "$NS_B" addr add 10.10.2.2/24 dev b-to-r2
  ip -n "$NS_B" link set b-to-r2 up
  ip -n "$NS_B" route add default via 10.10.2.1
  ip -n "$NS_R2" addr add 10.10.2.1/24 dev r2-to-b
  ip -n "$NS_R2" addr add 198.51.100.2/24 dev r2-to-i
  ip -n "$NS_R2" link set r2-to-b up
  ip -n "$NS_R2" link set r2-to-i up
  if [[ "$use_v6" -eq 1 ]]; then
    ip -n "$NS_A" -6 addr add 2001:77:1::2/64 dev a-to-r1
    ip -n "$NS_A" -6 route add default via 2001:77:1::1
    ip -n "$NS_R1" -6 addr add 2001:77:1::1/64 dev r1-to-a
    ip -n "$NS_R1" -6 addr add 2001:77:ff::1/64 dev r1-to-c
    ip -n "$NS_B" -6 addr add 2001:77:2::2/64 dev b-to-r2
    ip -n "$NS_B" -6 route add default via 2001:77:2::1
    ip -n "$NS_R2" -6 addr add 2001:77:2::1/64 dev r2-to-b
    ip -n "$NS_R2" -6 addr add 2001:77:ee::2/64 dev r2-to-i
    ip -n "$NS_INET" -6 addr add 2001:77:ee::1/64 dev i-to-r2
    # v6 routes belong HERE: they are only installable once the global v6
    # addresses above exist — running them unconditionally broke every v4
    # case (run 43266120269: "r1 v6 route install failed" on pcone-pcone).
    # akinet is persistent: replace. r1/r2 are fresh: add. All checked.
    ip -n "$NS_R1" -6 route add 2001:77:2::/64 via 2001:77:ff::254       || { log "FAIL: r1 v6 route install failed"; return 1; }
    ip -n "$NS_R2" -6 route add default via 2001:77:ee::1       || { log "FAIL: r2 v6 route install failed"; return 1; }
    ip -n "$NS_INET" -6 route replace 2001:77:1::/64 via 2001:77:ff::1       || { log "FAIL: inet v6 route (alice LAN) failed"; return 1; }
    ip -n "$NS_INET" -6 route replace 2001:77:2::/64 via 2001:77:ee::2       || { log "FAIL: inet v6 route (bob LAN) failed"; return 1; }
  fi
  # Routing: endpoints default to their router; r1 reaches bob's LAN via
  # the "public internet" hop; r2 defaults to it; inet routes both LANs;
  # svc returns to alice directly and to bob via the internet hop.
  # G0 fix: routes to the PEER LAN (below) are not enough — once r2
  # MASQUERADEs bob's sources to 198.51.100.2, replies (STUN/registry) and
  # alice's punches to bob's reflexive address need 198.51.100.0/24 routes
  # on r1 AND svc too, or the NAT'd round trips die after the smoke.
  must "r1 route to bob LAN"   ip -n "$NS_R1" route add 10.10.2.0/24 via 203.0.113.254 || return 1
  must "r1 route to bob NAT"   ip -n "$NS_R1" route add 198.51.100.0/24 via 203.0.113.254 || return 1
  must "svc route to bob NAT"  ip -n "$NS_SVC" route replace 198.51.100.0/24 via 203.0.113.254 || return 1
  must "r2 route to core"      ip -n "$NS_R2" route add 203.0.113.0/24 via 198.51.100.254 || return 1
  ip -n "$NS_R2" route add 10.10.1.0/24 via 198.51.100.254
  # akinet is persistent across cases: its routes are re-asserted every
  # case, so they must be idempotent (plain `add` produced "File exists"
  # from case 2 onward — run 43253690898 logs, 3 occurrences).
  must "inet route to alice LAN" ip -n "$NS_INET" route replace 10.10.1.0/24 via 203.0.113.1 || return 1
  must "inet route to bob LAN"   ip -n "$NS_INET" route replace 10.10.2.0/24 via 198.51.100.2 || return 1
  for ns in "$NS_R1" "$NS_R2"; do
    ip netns exec "$ns" sysctl -qw net.ipv4.ip_forward=1
    ip netns exec "$ns" sysctl -qw net.ipv6.conf.all.forwarding=1
  done
  # The services host needs return routes to both LANs: the baseline
  # smoke runs BEFORE the NAT rules, so alice/bob traffic arrives with
  # its ORIGINAL source address and svc would drop the reply (no
  # route) — exactly the first CI run's "base topology broken".
  # (After MASQUERADE the sources become on-link router addresses,
  # but the smoke must pass pre-NAT. Bob's LAN now returns via akinet.)
  ip -n "$NS_SVC" route replace 10.10.1.0/24 via 203.0.113.1
  ip -n "$NS_SVC" route replace 10.10.2.0/24 via 203.0.113.254
  # Reverse-path filtering off everywhere: the punch traffic this suite
  # exists to test arrives from addresses the endpoint never talked to
  # (the peer's NAT reflection), which strict rp_filter would drop as
  # spoofing — the exact false negative the matrix must not have.
  for ns in "$NS_A" "$NS_B" "$NS_R1" "$NS_R2" "$NS_SVC"; do
    ip netns exec "$ns" sysctl -qw net.ipv4.conf.all.rp_filter=0
    ip netns exec "$ns" sysctl -qw net.ipv4.conf.default.rp_filter=0
  done
  # Baseline connectivity smoke: both endpoints reach the services host.
  ip netns exec "$NS_A" ping -c1 -W2 203.0.113.10 >/dev/null 2>&1 \
    || { log "FAIL: alice cannot reach the services host (base topology broken)"; dump_routes; return 1; }
  ip netns exec "$NS_B" ping -c1 -W2 203.0.113.10 >/dev/null 2>&1 \
    || { log "FAIL: bob cannot reach the services host (base topology broken)"; dump_routes; return 1; }
}

# nat_rules <router-ns> <lan-iface> <core-iface> <lan-subnet> <mode>
#   mode: pcone | fullcone | symmetric | control-only
# The stateful base (outbound NEW allowed, inbound only ESTABLISHED) IS the
# port-restricted filtering; the modes add the mapping flavor.
nat_rules() {
  local ns="$1" lan="$2" core="$3" subnet="$4" mode="$5"
  # G0 hardening: every iptables mutation is checked (finding C) — a
  # silently-failed flush/append would run the case under rules that do
  # not model the intended NAT at all.
  ip netns exec "$ns" iptables -P FORWARD DROP || return 1
  ip netns exec "$ns" iptables -F FORWARD || return 1
  ip netns exec "$ns" iptables -F -t nat || return 1
  ip netns exec "$ns" iptables -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT || return 1
  case "$mode" in
    pcone)
      ip netns exec "$ns" iptables -A FORWARD -i "$lan" -j ACCEPT || return 1
      ip netns exec "$ns" iptables -t nat -A POSTROUTING -s "$subnet" -o "$core" -j MASQUERADE || return 1
      ;;
    fullcone)
      # 1:1 static DNAT (port-preserving) + unconditional inbound UDP to the
      # LAN host: any outsiders may send to the published endpoint = full cone.
      local host_ip
      host_ip="${subnet%0/24}2"
      ip netns exec "$ns" iptables -A FORWARD -i "$lan" -j ACCEPT
      ip netns exec "$ns" iptables -t nat -A POSTROUTING -s "$subnet" -o "$core" -j MASQUERADE
      ip netns exec "$ns" iptables -t nat -A PREROUTING -i "$core" -p udp -j DNAT --to-destination "$host_ip"
      ip netns exec "$ns" iptables -A FORWARD -i "$core" -p udp -d "$host_ip" -j ACCEPT
      ;;
    symmetric)
      ip netns exec "$ns" iptables -A FORWARD -i "$lan" -j ACCEPT || return 1
      ip netns exec "$ns" iptables -t nat -A POSTROUTING -s "$subnet" -o "$core" -j MASQUERADE --random-fully || return 1
      ;;
    control-only)
      # v6 case: the v4 data plane must die (peer dials are not to the
      # services host), while the control plane (registry/STUN, always
      # initiated outbound toward 203.0.113.10) stays alive. No MASQUERADE:
      # svc holds explicit return routes.
      ip netns exec "$ns" iptables -A FORWARD -i "$lan" -d 203.0.113.10 -j ACCEPT
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

# v6_blocked <router-ns>: IPv4 cases must not leak v6 past the NAT under
# test even if some future edit re-adds endpoint v6 addresses (belt to
# the suspenders of not configuring them at all).
v6_blocked() {
  ip netns exec "$1" ip6tables -P FORWARD DROP
  ip netns exec "$1" ip6tables -F FORWARD
}

# ---------------------------------------------------------------------------
# 4. Services: the real registry + TWO standalone STUN responders (the
#    second is the fixture self-check's second destination), all inside
#    the services namespace.
# ---------------------------------------------------------------------------
REG_PORT=17300
STUN_PORT=17101
STUN_PORT2=17102
start_services() {
  mkdir -p "$WORK/svc-id"
  ip netns exec "$NS_SVC" "$CLI" registry-server \
    --port "$REG_PORT" --db-path "$WORK/registry.db" --id-dir "$WORK/svc-id" \
    >"$WORK/registry.log" 2>&1 &
  PIDS_REG=$!
  ip netns exec "$NS_SVC" "$CLI" stun-server --port "$STUN_PORT" \
    >"$WORK/stun.log" 2>&1 &
  PIDS_STUN="$!"
  ip netns exec "$NS_SVC" "$CLI" stun-server --port "$STUN_PORT2" \
    >"$WORK/stun2.log" 2>&1 &
  PIDS_STUN="$PIDS_STUN $!"
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
    grep -q "stun responder" "$WORK/stun.log" 2>/dev/null \
      && grep -q "stun responder" "$WORK/stun2.log" 2>/dev/null && break
    sleep 0.2; waited=$((waited + 1))
  done
  grep -q "stun responder" "$WORK/stun.log" 2>/dev/null \
    || { log "FAIL: stun-server did not come up (log: $(tail -3 "$WORK/stun.log" 2>/dev/null))"; return 1; }
  grep -q "stun responder" "$WORK/stun2.log" 2>/dev/null \
    || { log "FAIL: second stun-server did not come up (fixture self-check destination)"; return 1; }
  log "services up: registry 203.0.113.10:$REG_PORT (fp ${REG_FP:0:8}…), stun :$STUN_PORT + :$STUN_PORT2 (fixture probe)"
}

# ---------------------------------------------------------------------------
# 5. Fixture self-check (CR-P2-14 point 3): from ONE UDP socket inside an
#    endpoint namespace, query the TWO STUN destinations; the two
#    XOR-MAPPED ports reveal the router's mapping behavior:
#      pcone/fullcone -> SAME external port (endpoint-independent)
#      symmetric      -> DIFFERENT ports (per-destination random)
#    A router that does not behave as its mode claims makes every
#    downstream result meaningless — the case is judged "fixture
#    invalid", never a product pass/fail.
# ---------------------------------------------------------------------------
fixture_probe() { # fixture_probe <ns> -> prints "IP:PORT IP:PORT" or fails
  # G0 hardening: the previous parser accepted wrong txid / wrong source /
  # wrong type / wrong cookie / wrong family / zero-length headers /
  # truncated XOR-MAPPED bodies, reused replies across destinations, and
  # rejected valid padded responses (attribute walk ignored STUN 4-byte
  # alignment). It printed only ports, so a probe that bypassed the NAT
  # (seeing the LAN address) could pass the same-port check trivially.
  # This parser validates everything and returns BOTH mapped addresses.
  ip netns exec "$1" python3 - <<'PY'
import os, socket, struct, sys

MAGIC = 0x2112A442
BINDING_REQUEST = 0x0001
BINDING_RESPONSE = 0x0101
XOR_MAPPED = 0x0020

def parse_response(data, txid, peer):
    if len(data) < 20:
        return None
    mtype, mlen, cookie = struct.unpack("!HHI", data[:8])
    if mtype != BINDING_RESPONSE or cookie != MAGIC or data[8:20] != txid:
        return None
    if len(data) < 20 + mlen:  # declared length vs datagram bounds
        return None
    off = 20
    while off + 4 <= len(data):
        atype, alen = struct.unpack("!HH", data[off:off + 4])
        body = data[off + 4:off + 4 + alen]
        if len(body) < alen:  # attribute extends past the message
            return None
        if atype == XOR_MAPPED and alen >= 8:
            fam = body[1]
            if fam != 0x01:  # IPv4 only in this fixture
                return None
            xport = struct.unpack("!H", body[2:4])[0] ^ (MAGIC >> 16)
            xip = bytes(b ^ k for b, k in zip(body[4:8], struct.pack("!I", MAGIC)))
            return socket.inet_ntoa(xip), xport
        off += 4 + ((alen + 3) & ~3)  # STUN attributes are 4-byte aligned
    return None

def query(sock, addr):
    txid = os.urandom(12)
    msg = struct.pack("!HHI", BINDING_REQUEST, 0, MAGIC) + txid
    for _ in range(4):
        sock.sendto(msg, addr)
        sock.settimeout(1.0)
        try:
            data, src = sock.recvfrom(560)
        except socket.timeout:
            continue
        if src[0] != addr[0]:  # a late reply from the OTHER destination
            continue           # must not be misattributed (G0 finding E)
        parsed = parse_response(data, txid, addr)
        if parsed is not None:
            return parsed
    return None

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
r1 = query(s, ("203.0.113.10", 17101))
r2 = query(s, ("203.0.113.10", 17102))
if r1 is None or r2 is None:
    print("PROBE-FAIL")
    sys.exit(1)
print(f"{r1[0]}:{r1[1]} {r2[0]}:{r2[1]}")
PY
}

# fixture_check <ns> <mode>: judges the probe against the mode's claim.
fixture_check() {
  local ns="$1" mode="$2" out a1 p1 a2 p2
  out="$(fixture_probe "$ns")" || { log "FAIL: fixture probe ($ns) got no STUN answers"; return 1; }
  read -r a1 p1 a2 p2 <<<"$out"  # two "IP:PORT" results — wait, split below
  # out is "IP1:PORT1 IP2:PORT2"; split each field
  local m1 m2
  m1="${out%% *}"; m2="${out##* }"
  a1="${m1%%:*}"; p1="${m1##*:}"
  a2="${m2%%:*}"; p2="${m2##*:}"
  log "    fixture ($ns/$mode): same socket -> $m1 and $m2"
  # Address assertion by mode (G0 finding E): the observed address proves
  # whether the NAT is actually engaged — without it a probe bypassing
  # the NAT sees the LAN address and a same-port verdict proves nothing.
  local expected_addr
  case "$ns:$mode" in
    "$NS_A":pcone|"$NS_A":fullcone|"$NS_A":symmetric) expected_addr="203.0.113.1" ;;
    "$NS_B":pcone|"$NS_B":fullcone|"$NS_B":symmetric) expected_addr="198.51.100.2" ;;
    "$NS_A":control-only) expected_addr="10.10.1.2" ;;  # no v4 MASQ: LAN address
    "$NS_B":control-only) expected_addr="10.10.2.2" ;;
    *) expected_addr="" ;;
  esac
  if [[ -n "$expected_addr" ]]; then
    [[ "$a1" == "$expected_addr" && "$a2" == "$expected_addr" ]]       || { log "FAIL: fixture invalid — $ns/$mode must observe $expected_addr (NAT engagement), got $a1/$a2"; return 1; }
  fi
  # Port comparison by mode (the original CR-P2-14 point 3 semantics).
  case "$mode" in
    pcone|fullcone)
      [[ "$p1" == "$p2" ]] || { log "FAIL: fixture invalid — $mode must map one socket to ONE external port ($p1 != $p2)"; return 1; }
      ;;
    symmetric)
      [[ "$p1" != "$p2" ]] || { log "FAIL: fixture invalid — symmetric must remap per destination ($p1 == $p2)"; return 1; }
      ;;
    control-only)
      # No v4 NAT: ports are the socket's own — nothing more to assert.
      ;;
  esac
  return 0
}

# ---------------------------------------------------------------------------
# 6. One transfer case. run_case <name> <expect: pass-v4|xfail|fail|pass-v6>
#    [sender-env] [receiver-env] [recv-feed: accept|decline-first]
#    alice = `task code-recv` (receiver), bob = `task join-send` (sender).
#    The optional env prefixes (CR-P1-07, review 2026-11-13 §4.4 改法 4)
#    feed the HIDDEN DEBUG KNOBS the late-send / no-low-TTL / decline-retry
#    cases need (default off — an unset/empty prefix runs the plain
#    product; the knobs compile ONLY in exp-knobs builds, CR-P3-16):
#      AK_EXP_SEND_DELAY_MS=<ms>  sender: delay the send start after
#                                 pairing (the GUI's pick-files timing)
#      AK_EXP_NO_TTL=1            receiver: punch with NO low-TTL phase
#                                 (the pre-CR-P1-08 Windows shape)
#      AK_EXP_SEND_RETRY_MS=<ms>  sender: after a FAILED/DECLINED attempt,
#                                 wait this long and retry (the GUI's
#                                 "declined, user tries again later"
#                                 timing; the mapping keepalive resumes
#                                 within the remaining budget meanwhile —
#                                 CR-P3-15)
#    recv-feed selects the receiver's stdin shape: `accept` (default)
#    auto-accepts every consent prompt; `decline-first` answers the FIRST
#    prompt with `n` and every later one with `y` — the CLI discards
#    input that predates a prompt (R36-P2-01), so each answer is fed only
#    after its prompt appears in the receiver's log.
# ---------------------------------------------------------------------------
# Failure forensics (CR-P3-11): the 6832f79 commit message described
# this function and a conntrack sampler but neither was actually written.
# Both are now real: fail_case prints the FAIL line plus the receiver's
# log tail and the last conntrack snapshot taken during the dial.
fail_case() { # fail_case <message> — print + forensics, return 1
  log "$*"
  local case_dir="$1"
  # The caller's message is $*; the case_dir is derived from the case_name
  # variable in scope. Print receiver tail + last conntrack sample.
  log "    receiver log tail:"
  tail -8 "recv.log" 2>/dev/null | sed 's/^/      /' || true
  if [[ -f "conntrack-live.log" ]]; then
    log "    live conntrack (last window):"
    tail -40 "conntrack-live.log" | sed 's/^/      /'
  fi
  return 1
}

wait_for_text() { # wait_for_text <file> <needle> <timeout-secs> -> 0/1
  local file="$1" needle="$2" budget="$3" waited=0
  while [[ $waited -lt $((budget * 10)) ]]; do
    grep -q "$needle" "$file" 2>/dev/null && return 0
    sleep 0.1; waited=$((waited + 1))
  done
  return 1
}

# count_text <file> <needle> -> number of matching lines (0 on missing file)
count_text() {
  grep -c -- "$2" "$1" 2>/dev/null || true
}

# The decline-first receiver stdin feeder (CR-P3-15's matrix case): watch
# the receiver's log and answer each consent prompt as it appears — the
# FIRST prompt gets a confirmed `n`, the SECOND gets `y` (then `yes`
# keeps any further prompt accepted; the first decline is never masked).
# Two timing facts shape it:
#   * the CLI's terminal input discards lines whose read completed before
#     the prompt's freshness boundary (R36-P2-01), and the boundary lands
#     a beat AFTER the prompt text hits the log — a write the instant the
#     line appears can still be discarded (J1 walkthrough measured it;
#     600 ms of settle after the observed prompt is the proven margin);
#   * every wait is BOUNDED (300 s each): a dead receiver ends the feeder
#     (its stdin then reads EOF and the CLI fail-closed declines) instead
#     of leaking an infinite loop past the case.
decline_first_feeder() { # owns the consent pipe and its polling children
  trap finish_worker EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  local log="$1" waited=0
  local bound=1500  # 300 s in 0.2 s polls
  while [[ "$(count_text "$log" 'accept? \[Y/n\]')" -lt 1 ]]; do
    worker_command sleep 0.2; waited=$((waited + 1)); [[ $waited -lt $bound ]] || exit 0
  done
  worker_command sleep 0.6
  printf 'n\n'
  # The first decline must actually LAND (the receiver prints its
  # "offer declined; still listening" line) before the second wait
  # starts — the scenario needs a real first rejection, not a lost one.
  waited=0
  while ! grep -q 'offer declined' "$log" 2>/dev/null; do
    worker_command sleep 0.2; waited=$((waited + 1)); [[ $waited -lt $bound ]] || exit 0
  done
  waited=0
  while [[ "$(count_text "$log" 'accept? \[Y/n\]')" -lt 2 ]]; do
    worker_command sleep 0.2; waited=$((waited + 1)); [[ $waited -lt $bound ]] || exit 0
  done
  worker_command sleep 0.6
  printf 'y\n'
  exec yes
}

# CR-P3-15 retry-evidence audit (integration review of batch J): parse
# the SENDER's actually-measured evidence lines and assert the scenario
# really happened — the first attempt ended DECLINED, the retry started
# >= min_ms (monotonic ms since the first failure, measured by the CLI,
# not a config echo), the retry COMPLETED, and both attempts rode the
# SAME pairing (exactly one "paired;" line). Prints the de-sensitized
# summary line for CI-log consumption on success (case name + numbers +
# the bounded class word only — no codes, keys, id dirs or diagnostics).
assert_retry_evidence() { # assert_retry_evidence <case_name> <send.log> <min_ms>
  local case_name="$1" send_log="$2" min_ms="$3"
  local line since_ms
  line="$(grep -m1 'retry-evidence attempt=2 since_failure_ms=' "$send_log" 2>/dev/null)" \
    || { log "FAIL [$case_name]: no retry-evidence timing line in the sender log"; return 1; }
  since_ms="$(printf '%s' "$line" | sed -n 's/.*since_failure_ms=\([0-9]*\).*/\1/p')"
  [[ -n "$since_ms" ]] || { log "FAIL [$case_name]: unparseable since_failure_ms: $line"; return 1; }
  [[ "$since_ms" -ge "$min_ms" ]] \
    || { log "FAIL [$case_name]: retry gap ${since_ms}ms < ${min_ms}ms (the knob did not actually wait)"; return 1; }
  grep -q 'retry-evidence attempt=2 since_failure_ms=.* first_failure=declined' "$send_log" \
    || { log "FAIL [$case_name]: the first attempt did not end declined (evidence: $(grep -m1 'first_failure=' "$send_log" || echo missing))"; return 1; }
  grep -q 'retry-evidence attempt=2 outcome=completed' "$send_log" \
    || { log "FAIL [$case_name]: the retry never completed"; return 1; }
  local pairings
  pairings="$(count_text "$send_log" 'paired; peer')"
  [[ "$pairings" -eq 1 ]] \
    || { log "FAIL [$case_name]: expected exactly one pairing for both attempts, saw $pairings"; return 1; }
  log "[retry-evidence] case=$case_name first=declined same_pairing=true delay_ms=$since_ms retry=completed"
  return 0
}

print_via() { # connected-via of a case, for the log regardless of outcome
  local via
  via="$(grep -m1 '\[task\] connected via ' "$1" 2>/dev/null | awk '{print $NF}')"
  if [[ -n "$via" ]]; then
    log "    connected-via: $via"
  else
    # The CLI prints this marker only at Attempt::Done — its absence means
    # the success marker is missing, NOT that no connection ever existed.
    log "    connected-via: (no success marker — Attempt::Done never printed)"
  fi
}

run_case() {
  # NOTE: split declarations — under `set -u`, one `local` line expands
  # ALL its words BEFORE assigning, so a later field referencing an
  # earlier one would see it unbound (the round-2 CI failure).
  local case_name="$1" expect="$2" sender_env="${3:-}" recv_env="${4:-}" recv_feed="${5:-accept}"
  local case_dir="$WORK/$case_name"
  mkdir -p "$case_dir/id-alice" "$case_dir/id-bob" "$case_dir/out" "$case_dir/src"
  # G0 hardening: payload generation must SUCCEED — a failed dd previously
  # produced an empty/missing file whose "clean failure" could satisfy the
  # xfail wording check (judgment-layer finding F).
  dd if=/dev/urandom of="$case_dir/src/payload.bin" bs=1M count="$SIZE_MB" status=none     || { log "FAIL [$case_name]: dd failed to generate the payload (input invalid — never a valid xfail)"; return 1; }
  [[ -s "$case_dir/src/payload.bin" ]]     || { log "FAIL [$case_name]: payload file empty after dd"; return 1; }

  # The receiver: consent auto-fed (default: a fresh 'y' stream — the CLI
  # discards stale input; `yes` keeps supplying fresh lines. The
  # decline-first feeder answers the first prompt with `n` and later ones
  # with `y`, CR-P3-15's case). The optional env prefix (CR-P1-07 knob,
  # unquoted word-split by design: "VAR=VAL" or empty) rides ahead of the
  # namespace exec.
  run_receiver() { # stdin = the consent feed; env prefix word-splits by design
    exec env $recv_env ip netns exec "$NS_A" "$CLI" --id-dir "$case_dir/id-alice" task code-recv \
        --out "$case_dir/out" \
        --server "203.0.113.10:$REG_PORT" --registry-fp "$REG_FP" \
        --stun "203.0.113.10:$STUN_PORT" \
        >"$case_dir/recv.log" 2>&1
  }
  local feed_fd feed_pid
  if [[ "$recv_feed" == "decline-first" ]]; then
    exec {feed_fd}< <(decline_first_feeder "$case_dir/recv.log")
  else
    exec {feed_fd}< <(exec yes)
  fi
  feed_pid=$!
  PIDS_KIDS="$PIDS_KIDS $feed_pid"
  run_receiver <&"$feed_fd" &
  local recv_pid=$!
  exec {feed_fd}<&-
  PIDS_KIDS="$PIDS_KIDS $recv_pid"
  wait_for_text "$case_dir/recv.log" "AKI1-" 120 \
    || { log "FAIL [$case_name]: receiver never printed a code"; return 1; }
  local code
  code="$(grep -o 'AKI1-[0-9A-Z-]*' "$case_dir/recv.log" | head -1)"
  [[ -n "$code" ]] || { log "FAIL [$case_name]: no parsable code"; return 1; }

  # The sender (same optional env prefix — the late-send knob lives on
  # the sender side).
  env $sender_env ip netns exec "$NS_B" "$CLI" --id-dir "$case_dir/id-bob" task join-send \
      "$code" "$case_dir/src/payload.bin" \
      --server "203.0.113.10:$REG_PORT" --registry-fp "$REG_FP" \
      --stun "203.0.113.10:$STUN_PORT" \
      >"$case_dir/send.log" 2>&1 &
  local send_pid=$!
  PIDS_KIDS="$PIDS_KIDS $send_pid"

  # Conntrack sampler (CR-P3-11): UDP entries expire ~30s unreplied, so a
  # post-mortem dump after a 240s case sees nothing. Sample both routers
  # every 5s while the sender runs; on failure fail_case shows the last
  # snapshot, on success the samples are discarded.
  (
    trap finish_worker EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    while kill -0 "$send_pid" 2>/dev/null; do
      printf '=== %(%H:%M:%S)T\n' -1 >>"$case_dir/conntrack-live.log"
      for ns in "$NS_R1" "$NS_R2"; do
        printf -- '--- %s\n' "$ns" >>"$case_dir/conntrack-live.log"
        worker_command ip netns exec "$ns" sh -c 'if command -v conntrack >/dev/null 2>&1; then exec conntrack -L -p udp 2>/dev/null; else exec grep -a "udp" /proc/net/nf_conntrack 2>/dev/null; fi' >>"$case_dir/conntrack-live.log" 2>&1
      done
      worker_command command sleep 5
    done
  ) &
  local sampler_pid=$!
  PIDS_KIDS="$PIDS_KIDS $sampler_pid"

  # Bounded wait for the sender to exit (fail cases legitimately take ~2
  # minutes of bounded retries; pass cases seconds; a hang is never OK).
  local waited=0
  while kill -0 "$send_pid" 2>/dev/null && [[ $waited -lt 2400 ]]; do
    sleep 0.1; waited=$((waited + 1))
  done
  if kill -0 "$send_pid" 2>/dev/null; then
    log "FAIL [$case_name]: sender did not finish within 240s (hang — never acceptable)"
    stop_case_children || true
    return 1
  fi
  wait "$send_pid"
  local send_rc=$?
  # Let the receiver finish its own teardown (bounded), then record HOW it
  # ended: a controlled exit (observed before the budget) is normal; a
  # timeout-kill is flagged — it is either a slow-but-fine teardown or a
  # hang, and the flag keeps the log honest about which happened without
  # claiming every kill is clean (judgment-layer finding F/d).
  local rwait=0
  while kill -0 "$recv_pid" 2>/dev/null && [[ $rwait -lt 150 ]]; do
    sleep 0.2; rwait=$((rwait + 1))
  done
  if kill -0 "$recv_pid" 2>/dev/null; then
    stop_pids "$recv_pid" || { log "FAIL [$case_name]: receiver cleanup exceeded its bound"; return 1; }
    log "note [$case_name]: receiver teardown exceeded 30s and was killed (slow teardown or hang — not judged as a controlled exit)"
  else
    wait "$recv_pid" 2>/dev/null || true
  fi
  stop_case_children || return 1

  local send_log="$case_dir/send.log" recv_log="$case_dir/recv.log"
  if [[ "$expect" == "pass-v4" || "$expect" == "pass-v6" ]]; then
    grep -q "\[task\] done:" "$send_log" \
      || { log "FAIL [$case_name]: sender did not complete (rc=$send_rc)"; tail -5 "$send_log" | sed 's/^/    /'; (cd "$case_dir" && fail_case "forensics") 2>/dev/null; return 1; }
    grep -q "\[task\] received" "$recv_log" \
      || { log "FAIL [$case_name]: receiver did not complete"; tail -5 "$recv_log" | sed 's/^/    /'; (cd "$case_dir" && fail_case "forensics") 2>/dev/null; return 1; }
    grep -q "\[task\] connected via " "$send_log" \
      || { log "FAIL [$case_name]: no connected-via line"; tail -5 "$send_log" | sed 's/^/    /'; return 1; }
    local via
    via="$(grep -m1 '\[task\] connected via ' "$send_log" | awk '{print $NF}')"
    if [[ "$expect" == "pass-v6" ]]; then
      [[ "$via" == *:* && "$via" != *.* ]] \
        || { log "FAIL [$case_name]: expected the IPv6 leg, carried by '$via'"; return 1; }
    else
      # pass-v4: the ride MUST be the peer router's public (core) address —
      # a private 10.10.1.x/10.10.2.x or a v6 form means the case is not
      # proving what it claims (CR-P2-14 point 1).
      [[ "$via" == 203.0.113.1:* ]] \
        || { log "FAIL [$case_name]: expected the v4 reflexive leg via alice's router (203.0.113.1), got '$via'"; return 1; }
    fi
    local sum_src sum_dst
    sum_src="$(sha256sum "$case_dir/src/payload.bin" | awk '{print $1}')"
    sum_dst="$(sha256sum "$case_dir/out/payload.bin" 2>/dev/null | awk '{print $1}')"
    [[ -n "$sum_dst" && "$sum_src" == "$sum_dst" ]] \
      || { log "FAIL [$case_name]: payload sha256 mismatch (src=$sum_src dst=$sum_dst)"; return 1; }
    # CR-P3-15: the decline-retry scenario's machine-verifiable audit —
    # only AFTER the byte-identical check (the summary line certifies
    # first=declined + same-pairing + measured monotonic gap + completed
    # retry on the ACTUAL run, never a config echo).
    if [[ "$recv_feed" == "decline-first" ]]; then
      assert_retry_evidence "$case_name" "$send_log" 40000 || return 1
    fi
    log "PASS [$case_name]: direct transfer, ${SIZE_MB}MiB byte-identical via $via"
  elif [[ "$expect" == "xfail" ]]; then
    # Strict expected-failure (CR-P2-14 point 6): the case must fail the
    # documented way while CR-P1-06 is open — and an unexpected PASS is a
    # RED (the expectation has become stale: flip the case to pass-v4).
    if grep -q "\[task\] done:" "$send_log"; then
      print_via "$send_log"
      log "FAIL [$case_name]: UNEXPECTEDLY PASSED — this case is marked xfail for CR-P1-06; if that is fixed, flip the expectation to pass-v4"
      return 1
    fi
    [[ $send_rc -ne 0 ]] \
      || { log "FAIL [$case_name]: xfail case exited 0 without completing"; return 1; }
    grep -q "no candidate accepted the connection" "$send_log" \
      || { log "FAIL [$case_name]: the xfail failure must be the bounded no-candidate wording (rc=$send_rc)"; tail -5 "$send_log" | sed 's/^/    /'; return 1; }
    if find "$case_dir/out" -type f 2>/dev/null | grep -q .; then
      log "FAIL [$case_name]: files landed despite the failed transfer (payload/.akpart/staging) — impossible without a relay (P0-class)"
      find "$case_dir/out" -type f 2>/dev/null | head -5 | sed 's/^/      /'
      return 1
    fi
    print_via "$send_log"
    log "PASS [$case_name]: xfail held (CR-P1-06 still open) — clean bounded failure, no data moved"
  else
    [[ $send_rc -ne 0 ]] \
      || { log "FAIL [$case_name]: symmetric<->symmetric must NOT succeed (E3 territory)"; print_via "$send_log"; return 1; }
    grep -q "no candidate accepted the connection" "$send_log" \
      || { log "FAIL [$case_name]: the failure must be the bounded no-candidate wording (rc=$send_rc)"; tail -5 "$send_log" | sed 's/^/    /'; return 1; }
    if find "$case_dir/out" -type f 2>/dev/null | grep -q .; then
      log "FAIL [$case_name]: files landed despite the failed transfer (payload/.akpart/staging) — impossible without a relay (P0-class)"
      find "$case_dir/out" -type f 2>/dev/null | head -5 | sed 's/^/      /'
      return 1
    fi
    print_via "$send_log"
    log "PASS [$case_name]: symmetric<->symmetric failed cleanly (bounded wording, no hang, no data moved)"
  fi
  return 0
}

# ---------------------------------------------------------------------------
# 7. The matrix.
# ---------------------------------------------------------------------------
# CR-P3-13 (H3): a kernel without IPv6 (module absent, or ipv6.disable=1)
# makes the v6 case's namespace setup die at the first `-6 addr add` /
# route install — an ENVIRONMENT limit, not a product failure, so the
# case must SKIP with its reason like every other gate, never FAIL.
# Probe the root namespace's stack (netns inherit kernel-wide v6 state):
# `::1` on lo is the last address a healthy stack always has, and
# net.ipv6.conf.all.disable_ipv6=1 speaks for boot-time disabling.
ipv6_usable() {
  if [[ "$(sysctl -n net.ipv6.conf.all.disable_ipv6 2>/dev/null)" == "1" ]]; then
    return 1
  fi
  ip -6 addr show dev lo 2>/dev/null | grep -q 'inet6'
}

FAILURES=0
case_pass() { # case_pass <name> <desc> <r1-mode> <r2-mode> <v6:0|1> <expect> [sender-env] [receiver-env] [recv-feed]
  local name="$1" desc="$2" r1="$3" r2="$4" use_v6="$5" expect="$6" sender_env="${7:-}" recv_env="${8:-}" recv_feed="${9:-accept}"
  log "---- case $name: $desc"
  build_case_topology "$use_v6" || { log "FAIL [$name]: topology setup failed"; dump_routes; FAILURES=$((FAILURES + 1)); return; }
  nat_rules "$NS_R1" r1-to-a r1-to-c 10.10.1.0/24 "$r1"     || { log "FAIL [$name]: r1 NAT rules failed"; dump_routes; FAILURES=$((FAILURES + 1)); return; }
  nat_rules "$NS_R2" r2-to-b r2-to-i 10.10.2.0/24 "$r2"     || { log "FAIL [$name]: r2 NAT rules failed"; dump_routes; FAILURES=$((FAILURES + 1)); return; }
  if [[ "$use_v6" -eq 1 ]]; then
    v6_firewall "$NS_R1" r1-to-a
    v6_firewall "$NS_R2" r2-to-b
  else
    # CR-P2-14 point 1: no v6 leg may bypass the v4 NATs under test.
    v6_blocked "$NS_R1"
    v6_blocked "$NS_R2"
  fi
  # CR-P2-14 point 3: fixture self-check — the NATs must behave as their
  # mode claims before any product result counts.
  fixture_check "$NS_A" "$r1" || { log "FAIL [$name]: r1 fixture invalid"; dump_conntrack; FAILURES=$((FAILURES + 1)); return; }
  fixture_check "$NS_B" "$r2" || { log "FAIL [$name]: r2 fixture invalid"; dump_conntrack; FAILURES=$((FAILURES + 1)); return; }
  run_case "$name" "$expect" "$sender_env" "$recv_env" "$recv_feed" || {
    stop_case_children || exit 1
    dump_conntrack
    FAILURES=$((FAILURES + 1))
    return
  }
  stop_case_children || exit 1
}

build_core || { log "FAIL: core topology setup failed (kernel refusing namespaces/bridges?)"; exit 1; }
start_services || exit 1

# CR-P1-06 fixed (low-TTL first punch phase + sender reflexive opener +
# reflexive-first dial): pcone<->pcone over v4 must now connect via the
# peer router's reflexive address — expectation flipped from strict xfail
# to pass-v4.
case_pass "pcone-pcone"    "port-restricted <-> port-restricted — v4 reflexive leg (CR-P1-06 strike-3 fix)" pcone pcone 0 pass-v4
case_pass "pcone-fullcone" "port-restricted (alice) <-> full-cone DNAT (bob) — v4 reflexive leg" pcone fullcone 0 pass-v4
if ! command -v ip6tables >/dev/null 2>&1; then
  log "SKIP case v6-stateful: ip6tables not available on this host"
elif ! ipv6_usable; then
  log "SKIP case v6-stateful: no IPv6 stack on this host (no inet6 address on lo, or net.ipv6.conf.all.disable_ipv6=1) — environment limit, not a product failure (CR-P3-13)"
else
  case_pass "v6-stateful"  "IPv6 both sides behind stateful firewalls (v4 control-plane only)" control-only control-only 1 pass-v6
fi
case_pass "symm-symm"      "symmetric <-> symmetric (--random-fully; MUST FAIL cleanly)"        symmetric symmetric 0 fail

# CR-P1-07 (review 2026-11-13 §4.4 改法 4): the late-send tiers — the GUI's
# "pair, then pick files, then send" latency, driven through the hidden
# AK_EXP_SEND_DELAY_MS debug knob (default off; only the matrix sets it).
# The pairing-time opener fires at pairing and the keepalive refreshes
# both NAT mappings every 15 s until the send starts.
case_pass "late-send-5s"   "pcone<->pcone, send starts >=5s after pairing (GUI timing; CR-P1-07 keepalive)" pcone pcone 0 pass-v4 "AK_EXP_SEND_DELAY_MS=5000" ""
case_pass "late-send-40s"  "pcone<->pcone, send starts >=40s after pairing (past the 30s unreplied timeout; CR-P1-07)" pcone pcone 0 pass-v4 "AK_EXP_SEND_DELAY_MS=40000" ""

# CR-P1-08: the receiver's low-TTL punch phase disabled entirely (the
# hidden AK_EXP_NO_TTL knob — the pre-H1 Windows shape). Passes because
# the sender's pairing-time opener sends its own IPv4 packets with a LOW
# TTL (they open only the sender's NAT flow) and the knob receiver
# delays its whole punch past PUNCH_LOW_TTL_PHASE, so its normal-TTL
# punches meet an EXISTING sender-side flow instead of creating the
# inbound-only conntrack record.
case_pass "recv-no-lowttl" "pcone<->pcone, receiver WITHOUT the low-TTL punch phase (pre-CR-P1-08 Windows shape)" pcone pcone 0 pass-v4 "" "AK_EXP_NO_TTL=1"

# CR-P3-15 (review 2026-11-13-batchH §4): the decline-retry case — the
# receiver DECLINES the first offer (the decline-first stdin feeder; a
# decline is not a task terminal state, so the listener keeps serving),
# the sender's retry waits >=40 s (past the 30 s unreplied-conntrack
# timeout) driven by the hidden AK_EXP_SEND_RETRY_MS knob, and during
# the wait the mapping keepalive RESUMES within the pairing's remaining
# idle budget (the exact recovery the FFI runs in product behavior). The
# second attempt must ride the refreshed mappings: pass-v4 via the peer
# router's reflexive address. On success (AFTER the byte-identical
# sha256 check) assert_retry_evidence audits the sender's measured
# retry-evidence lines and prints the de-sensitized
# "[retry-evidence] case=decline-retry-40s first=declined
# same_pairing=true delay_ms=<measured> retry=completed" summary for
# the CI log (the case's WORK dir is RAII-deleted at exit — the stdout
# summary is the durable record; J5's streak runner collects it).
case_pass "decline-retry-40s" "pcone<->pcone, first offer DECLINED, send retried >=40s later (CR-P3-15 keepalive recovery)" pcone pcone 0 pass-v4 "AK_EXP_SEND_RETRY_MS=40000" "" "decline-first"

log "======================================"
if [[ $FAILURES -eq 0 ]]; then
  log "RESULT: all runnable matrix cases PASS (incl. fixture self-checks)"
  exit 0
fi
log "RESULT: $FAILURES matrix case(s) FAILED"
exit 1
