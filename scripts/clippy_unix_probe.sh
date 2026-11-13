#!/usr/bin/env bash
# Local unix-path clippy probe: run clippy for the WHOLE workspace against the
# Android cross target (aarch64-linux-android), where `cfg(unix)` is true, so
# every unix-only branch actually compiles and gets linted BEFORE the Linux CI
# sees it. A green Windows-host `cargo clippy` does NOT cover those branches —
# on this host they are never type-checked.
#
# Motivation (the lesson that created this probe): commit 6235bec — the ubuntu
# clippy job broke on an E0599 that referenced an io::ErrorKind variant which
# only exists inside cfg(unix) code, while every Windows-local run was green
# because the cfg(unix) half of the tree never compiled locally. The same
# blind spot also hides unix-only lints, e.g. clippy::unnecessary_cast on
# `byte as u8` where c_char is u8 on aarch64 but i8 on other mainstream
# targets.
#
# Classification (same spirit as scripts/check_linux_targets.sh):
#   PASS    - clippy completed with zero errors on the android target;
#   SKIP    - host tooling missing (Android NDK or the rust target std).
#             This is an early-warning tool, not a release gate: SKIP is an
#             honest outcome, Linux CI remains the authority. Record
#             "not executed" per AGENTS.md when skipping;
#   FAIL    - clippy errors; a summary of messages + file locations is
#             printed (full log path included).
#
# Usage:   bash scripts/clippy_unix_probe.sh
# Env:     ANDROID_NDK_HOME (optional) NDK root override; defaults to
#          $USERPROFILE/android-toolchain/sdk/ndk/28.2.13676358
# Exit:    0 = PASS or SKIP; 1 = FAIL or script-level error.
# Logs:    target/clippy-unix-probe/clippy-aarch64-linux-android.log
set -euo pipefail

TARGET="aarch64-linux-android"

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
LOGDIR="$ROOT/target/clippy-unix-probe"
LOG="$LOGDIR/clippy-$TARGET.log"
mkdir -p "$LOGDIR"
cd "$ROOT"

skip() {
  echo "SKIP: $*"
  echo "      unix clippy probe not run — Linux CI remains the authority;"
  echo "      record 'not executed' per AGENTS.md if this gates your work."
  exit 0
}

# --- Tooling checks: missing NDK / target std is a SKIP, not a failure. ----
NDK_ROOT="${ANDROID_NDK_HOME:-${USERPROFILE:-$HOME}/android-toolchain/sdk/ndk/28.2.13676358}"
PREBUILT=""
for host_tag in windows-x86_64 linux-x86_64 darwin-x86_64; do
  if [ -d "$NDK_ROOT/toolchains/llvm/prebuilt/$host_tag/bin" ]; then
    PREBUILT="$NDK_ROOT/toolchains/llvm/prebuilt/$host_tag/bin"
    break
  fi
done
LINKER="" AR=""
if [ -n "$PREBUILT" ]; then
  if [ -e "$PREBUILT/aarch64-linux-android24-clang.cmd" ]; then
    LINKER="$PREBUILT/aarch64-linux-android24-clang.cmd" AR="$PREBUILT/llvm-ar.exe"
  elif [ -e "$PREBUILT/aarch64-linux-android24-clang" ]; then
    LINKER="$PREBUILT/aarch64-linux-android24-clang" AR="$PREBUILT/llvm-ar"
  fi
fi
if [ -z "$LINKER" ] || [ ! -e "$AR" ]; then
  skip "Android NDK toolchain not found under $NDK_ROOT
      set ANDROID_NDK_HOME to your NDK root (needs <ndk>/toolchains/llvm/
      prebuilt/<host>/bin with aarch64-linux-android24-clang{,.cmd} and llvm-ar)"
fi
if command -v rustup >/dev/null 2>&1; then
  if ! rustup target list --installed 2>/dev/null | grep -q "^$TARGET\$"; then
    skip "rust std for $TARGET not installed — run: rustup target add $TARGET"
  fi
fi

# --- The probe itself. -----------------------------------------------------
echo "==> unix clippy probe (ndk: $NDK_ROOT)"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$LINKER"
export CC_aarch64_linux_android="$LINKER"
export AR_aarch64_linux_android="$AR"

status=0
cargo clippy --locked --target "$TARGET" --workspace --all-targets -- -D warnings \
  >"$LOG" 2>&1 || status=$?

if [ "$status" -eq 0 ]; then
  echo "PASS: clippy clean on $TARGET (cfg(unix) branches all compiled there)"
  exit 0
fi

echo "FAIL: clippy reported errors on $TARGET (exit $status). Summary:"
# Pair each `error[...]` / `error:` line with the `--> file:line:col` line
# that follows it, and list the crates that failed to compile. (State-machine
# pairing, not getline: a "could not compile" line directly followed by the
# next error block must not swallow that error.)
awk '
  /^error/ && $0 !~ /could not compile/ { pending = $0; next }
  pending && /^ +--> / {
    loc = $0; sub(/^ +--> +/, "", loc)
    printf "  %s\n      at %s\n", substr(pending, 1, 140), loc
    pending = ""
  }
' "$LOG"
grep -E "^error: could not compile" "$LOG" | sed 's/^/  /' || true
echo
echo "Full log: $LOG"
echo "Reminder: these live in code paths the Windows host never compiles"
echo "(cfg(unix) branches — see commit 6235bec for why this probe exists)."
exit 1
