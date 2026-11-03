#!/usr/bin/env bash
# CI-only isolated test stacks. No host routing, firewall, forwarding or sysctl edits.
set -euo pipefail
PYTHON="$1"
LIBRARY="$2"
LEFT="ak-lan-$$-left"
RIGHT="ak-lan-$$-right"
VLEFT="akl$$"
VRIGHT="akr$$"
left_created=0
right_created=0
link_created=0
cleanup() {
  if [ "$link_created" = 1 ]; then ip link del "$VLEFT" 2>/dev/null || true; fi
  if [ "$left_created" = 1 ]; then ip netns del "$LEFT"; fi
  if [ "$right_created" = 1 ]; then ip netns del "$RIGHT"; fi
}
trap cleanup EXIT
ip netns add "$LEFT"; left_created=1
ip netns add "$RIGHT"; right_created=1
ip link add "$VLEFT" type veth peer name "$VRIGHT"; link_created=1
ip link set "$VLEFT" netns "$LEFT"
ip link set "$VRIGHT" netns "$RIGHT"
ip -n "$LEFT" addr add 10.203.0.1/30 dev "$VLEFT"
ip -n "$RIGHT" addr add 10.203.0.2/30 dev "$VRIGHT"
ip -n "$LEFT" link set lo up
ip -n "$RIGHT" link set lo up
ip -n "$LEFT" link set "$VLEFT" up
ip -n "$RIGHT" link set "$VRIGHT" up
# Neither isolated stack has a default route; no external server can be contacted.
test -z "$(ip -n "$LEFT" route show default)"
test -z "$(ip -n "$RIGHT" route show default)"
"$PYTHON" scripts/lan_process_smoke.py "$LIBRARY" \
  --left-ip 10.203.0.1 --right-ip 10.203.0.2 \
  --left-namespace "$LEFT" --right-namespace "$RIGHT"
