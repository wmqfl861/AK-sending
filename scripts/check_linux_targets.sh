#!/usr/bin/env bash
# Linux x86_64 cross-target `cargo check` for the FFI-facing crates.
# (Windows + Git Bash friendly; output format mirrors scripts/check_targets.sh.)
#
# Companion of the Linux Flutter runner (flutter_app/linux/, wave 24): the
# runner bundles libak_ffi.so, so the three crates behind that cdylib and its
# CLI/session consumers must stay checkable for x86_64-unknown-linux-gnu.
# scripts/check_targets.sh already covers the full 11-crate matrix on this
# triple; this script is the focused, fast lane for exactly the crates the
# Linux app ships:
#
#   cargo check --target x86_64-unknown-linux-gnu \
#               -p ak-ffi -p ak-cli -p ak-session
#
# Classification (same semantics as scripts/check_targets.sh):
#   PASS    - rustc checked all three crates for the triple;
#   BLOCKED - the only failures are custom build scripts of the C dependencies
#             (ring/blake3 need a target C compiler such as
#             x86_64-linux-gnu-gcc, which a Windows host does not have). Host
#             environment gap, NOT a code problem; the C-free subset
#             {ak-core, ak-wire} is checked as corroboration;
#   FAIL    - rustc compile errors pointing at crates/ak-* sources.
#
# On the current Windows host this is EXPECTED to report BLOCKED: a real
# Linux build needs a Linux host (or cross C toolchain + linker); see
# flutter_app/linux/BUILD.md. Native verification is delegated to Linux CI.
#
# Usage:   bash scripts/check_linux_targets.sh
# Exit:    0 = ran to completion with no FAIL (PASS / BLOCKED are honest
#          outcomes); 1 = FAIL or script-level error.
# Logs:    target/check-targets/check-<triple>{,-pure}.ffi.log
set -euo pipefail

TARGET="x86_64-unknown-linux-gnu"
PACKAGES="-p ak-ffi -p ak-cli -p ak-session"
PURE_PACKAGES="-p ak-core -p ak-wire"

die() { echo "ERROR: $*" >&2; exit 1; }

# cargo/rustup live in ~/.cargo/bin; prepend it when present (idempotent).
# USERPROFILE is Windows-only — plain Linux hosts don't define it, and
# reading it bare under `set -u` would abort the script before it starts.
if [ -n "${USERPROFILE:-}" ] && [ -d "$USERPROFILE/.cargo/bin" ]; then
  case ":$PATH:" in
    *":$USERPROFILE/.cargo/bin:"*) ;;
    *) export PATH="$USERPROFILE/.cargo/bin:$PATH" ;;
  esac
fi
command -v cargo >/dev/null 2>&1 \
  || die "cargo not found on PATH — install Rust or run: export PATH=\"\$USERPROFILE/.cargo/bin:\$PATH\""

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOGDIR="$ROOT/target/check-targets"
mkdir -p "$LOGDIR"
cd "$ROOT"

first_error_line() { # $1 = log path -> first `^error` line, truncated
  grep -m1 "^error" "$1" 2>/dev/null | cut -c1-120 || echo "(no error line captured)"
}

if ! rustup target list --installed | grep -qx "$TARGET"; then
  rustup target add "$TARGET" >/dev/null 2>&1 \
    || die "rustup target add failed for $TARGET"
fi

echo "==> cargo check ($TARGET): $PACKAGES"
log="$LOGDIR/check-$TARGET.ffi.log"
status="PASS" note=""
if cargo check $PACKAGES --target "$TARGET" >"$log" 2>&1; then
  status="PASS"
else
  # Real code problem = rustc compile errors that point at our crates.
  if grep -qE "error\[E[0-9]+\]" "$log" || grep -qE "crates[/\\]ak-" <(grep -E "^error(\[|\:)" "$log" || true); then
    status="FAIL"
    note="code error: $(first_error_line "$log")"
  elif grep -q "failed to run custom build command" "$log"; then
    status="BLOCKED"
    note="host env: $(first_error_line "$log")"
    # Corroborate: the C-free pure-Rust subset should still check. A
    # FAILURE here is a REAL code error (ak-core/ak-wire compile without
    # any C toolchain), so it must ESCALATE the whole run to FAIL — the
    # BLOCKED status only covers environment problems, and reporting
    # "blocked, non-failure" while the script itself observed rustc
    # errors would hide them (P2-12).
    if cargo check $PURE_PACKAGES --target "$TARGET" \
        >"$LOGDIR/check-$TARGET-pure.ffi.log" 2>&1; then
      note="$note; pure subset {ak-core,ak-wire} PASS"
    else
      status="FAIL"
      note="$note; pure subset {ak-core,ak-wire} FAIL: $(first_error_line "$LOGDIR/check-$TARGET-pure.ffi.log")"
    fi
  else
    status="FAIL"
    note="$(first_error_line "$log")"
  fi
fi
echo "    -> $status ${note:+($note)}"

echo
echo "==================== linux ffi check matrix ====================="
printf '%-28s %-8s %s\n' "TARGET" "STATUS" "FIRST ERROR / NOTE"
echo "-----------------------------------------------------------------"
printf '%-28s %-8s %s\n' "$TARGET" "$status" "$note"
echo "-----------------------------------------------------------------"
echo "PASS=$( [ "$status" = PASS ] && echo 1 || echo 0 ) \
BLOCKED=$( [ "$status" = BLOCKED ] && echo 1 || echo 0 ) \
SKIPPED=0 FAIL=$( [ "$status" = FAIL ] && echo 1 || echo 0 )"
echo "packages: ak-ffi ak-cli ak-session (subset of the scripts/"
echo "  check_targets.sh 11-crate matrix; default features only)"
echo "logs: $LOGDIR/check-$TARGET.ffi.log (full cargo output)"
echo "=================================================================="
[ "$status" != "FAIL" ] || exit 1
exit 0
