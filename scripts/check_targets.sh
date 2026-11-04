#!/usr/bin/env bash
# Cross-target `cargo check` portability matrix for the pure-Rust core.
# (Windows + Git Bash friendly; only Git Bash coreutils are used.)
#
# What one run does:
#   1. ensures the rust-std of each target triple is installed (rustup target
#      add; cargo check does NOT link, so no target linker is needed by Rust
#      itself -- but see the C caveat below);
#   2. always checks the three non-Android targets:
#        x86_64-unknown-linux-gnu  aarch64-apple-darwin  x86_64-apple-darwin
#   3. checks the two Android targets ONLY when an NDK can be located
#      ($ANDROID_NDK_HOME / $NDK_HOME, else autodiscovery under
#      $ANDROID_SDK_ROOT/ndk/<ver> or ~/android-toolchain/sdk/ndk/<ver>,
#      same layout as scripts/build_android_ffi.sh); otherwise prints the
#      reason and marks them SKIPPED;
#   4. for every target runs, exactly once:
#        cargo check -p ak-core -p ak-wire -p ak-identity -p ak-storage \
#                    -p ak-store -p ak-engine -p ak-signal -p ak-net \
#                    -p ak-session -p ak-cli -p ak-ffi --target <t>
#      (default features everywhere: the ak-signal `server-persistence`
#      opt-in rusqlite backend is deliberately NOT enabled here - it is
#      server-side and dev/test-only per crates/ak-signal/Cargo.toml and
#      crates/ak-session/Cargo.toml, and the summary table notes this)
#   5. classifies each non-passing target:
#        FAIL       - rustc reported compile errors (error[E...] or errors
#                     pointing at crates/ak-* sources): a REAL portability
#                     problem in our code; reported, never auto-fixed here;
#        BLOCKED    - the only failures are custom build scripts of the C
#                     dependencies (ring/blake3 need a target C compiler via
#                     the cc crate: e.g. x86_64-linux-gnu-gcc, or Apple SDK
#                     headers such as TargetConditionals.h). This is a host
#                     environment gap, NOT a code problem. As corroboration
#                     the script then checks the C-free pure-Rust subset
#                     {ak-core, ak-wire} on that triple.
#   6. prints a summary table (status + first error line per target).
#
# Caveat documented in docs/TECH_SELECTION.zh-CN.md: `cargo check` still RUNS
# dependency build scripts, so ring/blake3 C compilation happens even though
# nothing is linked. On hosts without the target C toolchain the full 11-crate
# check therefore stops in ring/blake3 while our crates stay unproven there;
# the {ak-core, ak-wire} subset plus zero `error[E...]` lines in the log is
# the local evidence used to classify those targets as environment-blocked.
# Native full-matrix verification for linux-gnu/apple is delegated to the
# `portability` CI job (.github/workflows/ci.yml).
#
# Usage:
#   bash scripts/check_targets.sh
# Exit code: 0 = matrix ran to completion with no FAIL (PASS / BLOCKED /
# SKIPPED all count as honest outcomes); 1 = at least one FAIL (code issue)
# or a script-level error. Logs: target/check-targets/check-<triple>.log.
# Tooling: Git Bash (bash, grep, sed, sort, ls) + cargo/rustup on PATH
# (~/.cargo/bin is prepended if present).

set -euo pipefail

# The 11-crate matrix defined by the portability task (ak-discovery and
# ak-itests intentionally excluded; the host CI `rust` job covers them).
# ak-session (wave 21) joined the matrix: default dependencies only.
PACKAGES="-p ak-core -p ak-wire -p ak-identity -p ak-storage -p ak-store -p ak-engine -p ak-signal -p ak-net -p ak-session -p ak-cli -p ak-ffi"
# Pure-Rust subset: no dependency with a C build script reaches these two,
# so they check on any triple that has rust-std installed.
PURE_PACKAGES="-p ak-core -p ak-wire"

CROSS_TARGETS="x86_64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin"
ANDROID_TARGETS="x86_64-linux-android aarch64-linux-android"
API=24

die() { echo "ERROR: $*" >&2; exit 1; }

# cargo/rustup live in ~/.cargo/bin; prepend it when present (idempotent).
if [ -d "$USERPROFILE/.cargo/bin" ]; then
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

# ---------------------------------------------------------------------------
# 1. Locate an NDK for the Android targets (skip with a reason if absent)
# ---------------------------------------------------------------------------
SDK="${ANDROID_SDK_ROOT:-$USERPROFILE/android-toolchain/sdk}"
NDK="${ANDROID_NDK_HOME:-${NDK_HOME:-}}"
NDK_WHY=""
if [ -z "$NDK" ]; then
  if [ -d "$SDK/ndk" ] && [ -n "$(ls "$SDK/ndk" 2>/dev/null)" ]; then
    # Highest version wins; `ls | head -1` would be alphabetical (27.x < 9.x).
    NDK="$SDK/ndk/$(ls "$SDK/ndk" | sort -V | tail -1)"
    NDK_WHY="autodiscovered under $SDK/ndk"
  fi
fi

ANDROID_ENV_READY=0
if [ -n "$NDK" ] && [ -d "$NDK/toolchains/llvm/prebuilt" ]; then
  HOST_TAG=""
  for tag in windows-x86_64 linux-x86_64 darwin-x86_64; do
    [ -d "$NDK/toolchains/llvm/prebuilt/$tag/bin" ] && HOST_TAG="$tag"
  done
  if [ -n "$HOST_TAG" ]; then
    NDK_BIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin"
    EXT=""
    [ "$HOST_TAG" = "windows-x86_64" ] && EXT=".cmd"
    X64_CLANG="$NDK_BIN/x86_64-linux-android$API-clang$EXT"
    ARM64_CLANG="$NDK_BIN/aarch64-linux-android$API-clang$EXT"
    if [ -f "$X64_CLANG" ] && [ -f "$ARM64_CLANG" ]; then
      # cc-rs env vars: ring/blake3 build scripts compile C even under
      # `cargo check`. The linker vars are exported too for parity with
      # scripts/build_android_ffi.sh (check itself does not link).
      export CC_x86_64_linux_android="$X64_CLANG"
      export CC_aarch64_linux_android="$ARM64_CLANG"
      export AR_x86_64_linux_android="$NDK_BIN/llvm-ar.exe"
      export AR_aarch64_linux_android="$NDK_BIN/llvm-ar.exe"
      [ "$HOST_TAG" != "windows-x86_64" ] && \
        AR_x86_64_linux_android="$NDK_BIN/llvm-ar" && \
        AR_aarch64_linux_android="$NDK_BIN/llvm-ar"
      export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$X64_CLANG"
      export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ARM64_CLANG"
      ANDROID_ENV_READY=1
      echo "==> Android NDK: $NDK (${NDK_WHY:-from env}), host tag $HOST_TAG, API $API"
    else
      NDK_WHY="NDK at $NDK lacks API-$API clang wrappers under $HOST_TAG"
    fi
  else
    NDK_WHY="NDK at $NDK has no prebuilt toolchain for this host"
  fi
else
  NDK_WHY="no NDK found (env ANDROID_NDK_HOME/NDK_HOME unset; nothing under $SDK/ndk)"
fi
if [ "$ANDROID_ENV_READY" -ne 1 ]; then
  echo "==> Android targets will be SKIPPED: $NDK_WHY"
  echo "    (ring/blake3 C build scripts need NDK clang even for cargo check;"
  echo "     see scripts/build_android_ffi.sh to install/point at an NDK)"
fi

# ---------------------------------------------------------------------------
# 2. Run the matrix
# ---------------------------------------------------------------------------
RESULTS=""  # lines of "target|status|note"
first_error_line() { # $1 = log path -> first `^error` line, truncated
  grep -m1 "^error" "$1" 2>/dev/null | cut -c1-120 || echo "(no error line captured)"
}

check_one_target() { # $1 = target triple
  local target="$1" log="$LOGDIR/check-$target.log" status note=""
  if ! rustup target list --installed | grep -qx "$target"; then
    rustup target add "$target" >/dev/null 2>&1 \
      || die "rustup target add failed for $target"
  fi
  echo "==> cargo check ($target): $PACKAGES"
  if cargo check $PACKAGES --target "$target" >"$log" 2>&1; then
    status="PASS"
  else
    # Real code problem = rustc compile errors that point at our crates.
    if grep -qE "error\[E[0-9]+\]" "$log" || grep -qE "crates[/\\]ak-" <(grep -E "^error(\[|\:)" "$log" || true); then
      status="FAIL"
      note="code error: $(first_error_line "$log")"
    elif grep -q "failed to run custom build command" "$log"; then
      status="BLOCKED"
      note="host env: $(first_error_line "$log")"
      # Corroborate: the C-free pure-Rust subset should still check.
      if cargo check $PURE_PACKAGES --target "$target" \
          >"$LOGDIR/pure-$target.log" 2>&1; then
        note="$note; pure subset {ak-core,ak-wire} PASS"
      else
        note="$note; pure subset {ak-core,ak-wire} FAIL: $(first_error_line "$LOGDIR/pure-$target.log")"
      fi
    else
      status="FAIL"
      note="$(first_error_line "$log")"
    fi
  fi
  echo "    -> $status ${note:+($note)}"
  RESULTS="$RESULTS
$target|$status|$note"
}

for target in $CROSS_TARGETS; do
  check_one_target "$target"
done
for target in $ANDROID_TARGETS; do
  if [ "$ANDROID_ENV_READY" -eq 1 ]; then
    check_one_target "$target"
  else
    echo "==> SKIPPED $target: $NDK_WHY"
    RESULTS="$RESULTS
$target|SKIPPED|$NDK_WHY"
  fi
done

# ---------------------------------------------------------------------------
# 3. Summary table
# ---------------------------------------------------------------------------
echo
echo "==================== portability check matrix ===================="
printf '%-28s %-8s %s\n' "TARGET" "STATUS" "FIRST ERROR / NOTE"
echo "-----------------------------------------------------------------"
tail -n +2 <<<"$RESULTS" | while IFS='|' read -r target status note; do
  [ -z "$target" ] && continue
  printf '%-28s %-8s %s\n' "$target" "$status" "$note"
done
# counts (the while loop above runs in a subshell; recount here)
fails=$(tail -n +2 <<<"$RESULTS" | awk -F'|' '$2 == "FAIL" {n++} END {print n+0}')
passes=$(tail -n +2 <<<"$RESULTS" | awk -F'|' '$2 == "PASS"  {n++} END {print n+0}')
blocked=$(tail -n +2 <<<"$RESULTS" | awk -F'|' '$2 == "BLOCKED"{n++} END {print n+0}')
skipped=$(tail -n +2 <<<"$RESULTS" | awk -F'|' '$2 == "SKIPPED"{n++} END {print n+0}')
echo "-----------------------------------------------------------------"
echo "PASS=$passes BLOCKED=$blocked SKIPPED=$skipped FAIL=$fails"
echo "feature status: ak-signal 'server-persistence' (opt-in rusqlite backend)"
echo "  NOT part of this matrix - server-side/dev-only per"
echo "  crates/ak-signal/Cargo.toml [features] and crates/ak-session"
echo "  [dev-dependencies]; every target above was checked with default"
echo "  features (ak-store file backend, no server SQLite). CI covers the"
echo "  feature on the host via: cargo test --locked -p ak-signal \\"
echo "  --features server-persistence (.github/workflows/ci.yml rust job)."
echo "logs: $LOGDIR/check-<triple>.log (full cargo output per target)"
echo "==================================================================="
[ "$fails" -eq 0 ] || exit 1
exit 0
