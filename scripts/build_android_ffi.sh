#!/usr/bin/env bash
# Cross-compile the ak-ffi cdylib for Android and stage it for the Flutter app
# (Windows + Git Bash friendly; only Git Bash coreutils are used).
#
# What one run does, end to end:
#   1. locates the Android SDK/NDK (default layout ~/android-toolchain/sdk,
#      NDK at sdk/ndk/<ver>; pick the highest version found);
#   2. exports per-target linker/compiler env vars pointing into the NDK
#      (pure environment-variable scheme: the NDK <ver> directory name cannot
#      be written statically into .cargo/config.toml, and env vars leave the
#      repo's cargo config untouched);
#   3. cargo build -p ak-ffi -p ak-session --release for
#      x86_64-linux-android (emulator) and aarch64-linux-android (arm64
#      devices). ak-session (wave 21) is the shared session layer B8's
#      pairing/scheduler FFI surface binds; its default dependency set
#      (ak-core, ak-identity, ak-signal default features, ak-store file
#      backend, blake3, log, thiserror, tokio) is fully cross-compilable
#      with the NDK clang exported below - the `server-persistence`
#      rusqlite backend is dev/test-only and never linked by a plain
#      `cargo build -p ak-session`;
#   4. copies both libak_ffi.so into
#      flutter_app/android/app/src/main/jniLibs/{x86_64,arm64-v8a}/;
#   5. verifies with the NDK's llvm-nm that each .so exports the frozen ABI
#      symbols ak_engine_new / ak_identity_info / ak_jobs_list plus the
#      v2 contract spot-check entries ak_pairing_start / ak_sched_listen_start
#      (docs/contracts/ffi-v2.md §1; strict: any missing symbol FAILS the
#      run and all missing ones are listed);
#   6. prints size + SHA-256 of every artifact.
#
# Usage:
#   bash scripts/build_android_ffi.sh [OPTIONS]
# Options:
#   --sdk <path>   Android SDK root (default: $ANDROID_SDK_ROOT / $ANDROID_HOME,
#                  or $ANDROID_SDK_ROOT if set)
#   --ndk <path>   NDK root, overriding the sdk/ndk/<ver> autodiscovery
#                  ($ANDROID_NDK_HOME / $NDK_HOME also honored)
#   --api <n>      Android API level for the clang wrappers (default: 24,
#                  the minimum shipping an NDK clang triple for all modern
#                  NDKs; Android 7.0+)
#   -h | --help    this text
#
# Exit code 0 = both .so built, verified and staged into jniLibs. On any
# missing prerequisite the script prints the expected paths and exits 1.
# Tooling: Git Bash (bash, ls, sort, grep, sed, cp, mkdir, stat, sha256sum)
# plus cargo/rustup on PATH (~/.cargo/bin is prepended if present).

set -euo pipefail

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
# Portable home: git-bash sets USERPROFILE, Linux CI does not.
HOMEISH="${USERPROFILE:-$HOME}"
SDK_DEFAULT="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOMEISH/android-toolchain/sdk}}"
SDK="$SDK_DEFAULT"
NDK="${ANDROID_NDK_HOME:-${NDK_HOME:-}}"
API=24

usage() { sed -n '2,35p' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --sdk) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
           SDK=$2; shift 2 ;;
    --ndk) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
           NDK=$2; shift 2 ;;
    --api) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
           API=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
done

die() { echo "ERROR: $*" >&2; exit 1; }

# cargo/rustup live in ~/.cargo/bin; prepend it when present (idempotent).
if [ -d "$HOMEISH/.cargo/bin" ]; then
  case ":$PATH:" in
    *":$HOMEISH/.cargo/bin:"*) ;;
    *) export PATH="$HOMEISH/.cargo/bin:$PATH" ;;
  esac
fi
command -v cargo >/dev/null 2>&1 \
  || die "cargo not found on PATH — install Rust or run: export PATH=\"\$HOME/.cargo/bin:\$PATH\""

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
JNILIBS="$ROOT/flutter_app/android/app/src/main/jniLibs"

# ---------------------------------------------------------------------------
# 1. Locate the SDK / NDK
# ---------------------------------------------------------------------------
if [ ! -d "$SDK" ]; then
  cat >&2 <<EOF
ERROR: Android SDK not found.
  Looked for: $SDK
  Expected layout (per docs/ANDROID_BUILD.zh-CN.md):
    \$USERPROFILE/android-toolchain/sdk/            (SDK root)
    \$USERPROFILE/android-toolchain/sdk/ndk/<ver>/  (an installed NDK)
  Fix: install the SDK+NDK, or pass --sdk <path> / set ANDROID_SDK_ROOT.
EOF
  exit 1
fi

if [ -z "$NDK" ]; then
  NDK_DIR="$SDK/ndk"
  if [ ! -d "$NDK_DIR" ]; then
    cat >&2 <<EOF
ERROR: no NDK found under the SDK.
  Looked for a directory under: $NDK_DIR
  Expected: $NDK_DIR/<version>/  (e.g. 27.2.12479018)
  Fix: install an NDK, e.g.
    sdkmanager --sdk_root="$SDK" --install "ndk;27.2.12479018"
  or pass --ndk <path> / set ANDROID_NDK_HOME.
EOF
    exit 1
  fi
  # Highest version wins; `ls | head -1` would be alphabetical (27.x < 9.x).
  NDK_VERSION="$(ls "$NDK_DIR" | sort -V | tail -1)"
  NDK="$NDK_DIR/$NDK_VERSION"
fi

# Host tag: windows-x86_64 on this dev box; linux-x86_64 kept as fallback so
# the same script runs inside WSL/Linux CI against a Linux-host NDK.
HOST_TAG=""
for tag in windows-x86_64 linux-x86_64; do
  [ -d "$NDK/toolchains/llvm/prebuilt/$tag/bin" ] && HOST_TAG="$tag"
done
if [ -z "$HOST_TAG" ]; then
  cat >&2 <<EOF
ERROR: NDK toolchain binaries not found.
  Looked under: $NDK/toolchains/llvm/prebuilt/{windows-x86_64,linux-x86_64}/bin
  Is '$NDK' a complete NDK installation? Reinstall it or pass --ndk <path>.
EOF
  exit 1
fi
NDK_BIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin"
echo "==> SDK:  $SDK"
echo "==> NDK:  $NDK"
echo "==> host: $HOST_TAG, API level $API"

# ---------------------------------------------------------------------------
# 2. Linker / compiler environment (per target; overrides nothing static)
#    NOTE: on Windows the NDK clang wrappers are .cmd scripts — cargo and
#    the cc crate both accept them directly as CC/LINKER values.
# ---------------------------------------------------------------------------
X64_CLANG="$NDK_BIN/x86_64-linux-android$API-clang.cmd"
ARM64_CLANG="$NDK_BIN/aarch64-linux-android$API-clang.cmd"
LLVM_AR="$NDK_BIN/llvm-ar.exe"
[ "$HOST_TAG" = "linux-x86_64" ] && LLVM_AR="$NDK_BIN/llvm-ar"

for tool in "$X64_CLANG" "$ARM64_CLANG" "$LLVM_AR"; do
  [ -f "$tool" ] || die "NDK tool not found: $tool
  The NDK at $NDK does not ship an API-$API clang wrapper. Try --api 21 or a newer NDK."
done

export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$X64_CLANG"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ARM64_CLANG"
# ak-store's bundled SQLite is compiled by the cc crate, which needs
# per-target CC/AR pointing at the same NDK tools.
export CC_x86_64_linux_android="$X64_CLANG"
export CC_aarch64_linux_android="$ARM64_CLANG"
export AR_x86_64_linux_android="$LLVM_AR"
export AR_aarch64_linux_android="$LLVM_AR"

# ---------------------------------------------------------------------------
# 3. rustup targets + cargo build (both ABIs in one invocation)
# ---------------------------------------------------------------------------
for target in x86_64-linux-android aarch64-linux-android; do
  rustup target list --installed | grep -qx "$target" || rustup target add "$target"
done

cd "$ROOT"
echo "==> cargo build -p ak-ffi -p ak-session --release (x86_64-linux-android + aarch64-linux-android)"
cargo build -p ak-ffi -p ak-session --release \
  --target x86_64-linux-android --target aarch64-linux-android

# ---------------------------------------------------------------------------
# 4. Stage into jniLibs (x86_64 = emulator, arm64-v8a = devices)
# ---------------------------------------------------------------------------
X64_SO="$ROOT/target/x86_64-linux-android/release/libak_ffi.so"
ARM64_SO="$ROOT/target/aarch64-linux-android/release/libak_ffi.so"
for so in "$X64_SO" "$ARM64_SO"; do
  [ -f "$so" ] || die "expected build output missing: $so"
done

mkdir -p "$JNILIBS/x86_64" "$JNILIBS/arm64-v8a"
cp -f "$X64_SO"   "$JNILIBS/x86_64/libak_ffi.so"
cp -f "$ARM64_SO" "$JNILIBS/arm64-v8a/libak_ffi.so"

# ---------------------------------------------------------------------------
# 5. Symbol verification with the NDK's llvm-nm (frozen ABI spot check)
# ---------------------------------------------------------------------------
LLVM_NM="$NDK_BIN/llvm-nm.exe"
[ "$HOST_TAG" = "linux-x86_64" ] && LLVM_NM="$NDK_BIN/llvm-nm"
[ -f "$LLVM_NM" ] || die "llvm-nm not found in NDK: $LLVM_NM"

# Frozen ABI v1 (ak_engine_new / ak_identity_info / ak_jobs_list) plus
# the v2 contract spot check (ak_pairing_start / ak_sched_listen_start)
# and, since R36-P2-07, the FULL task-plane set: all 8 ak_task_*
# exports (ffi-v2.md §8) — the old 5-symbol spot check let a pre-task
# .so pass while silently missing the entire v1-task-code surface.
# STRICT: every missing symbol is listed per .so and the run FAILs (no
# WARNING fallback).
REQUIRED_SYMBOLS="ak_engine_new ak_identity_info ak_jobs_list ak_pairing_start ak_sched_listen_start ak_task_start ak_task_create_code ak_task_join ak_task_send ak_task_list ak_task_disconnect ak_task_set_save_dir ak_task_consent"
verify_symbols() { # $1 = .so path, $2 = label
  local missing=""
  for sym in $REQUIRED_SYMBOLS; do
    if ! "$LLVM_NM" -D --defined-only "$1" | awk '{print $NF}' | grep -qx "$sym"; then
      missing="$missing $sym"
    fi
  done
  if [ -n "$missing" ]; then
    echo "ERROR: $2 ($1) is missing required ABI symbol(s):$missing" >&2
    echo "       (frozen ABI: ak_engine_new/ak_identity_info/ak_jobs_list; v2 contract: ak_pairing_start/ak_sched_listen_start)" >&2
    exit 1
  fi
}
verify_symbols "$JNILIBS/x86_64/libak_ffi.so"   "x86_64"
verify_symbols "$JNILIBS/arm64-v8a/libak_ffi.so" "arm64-v8a"
echo "==> symbols verified: $REQUIRED_SYMBOLS present in both .so"

# ---------------------------------------------------------------------------
# 6. Artifact report (size + SHA-256; target vs jniLibs must match)
# ---------------------------------------------------------------------------
for pair in "x86_64:$X64_SO" "arm64-v8a:$ARM64_SO"; do
  abi="${pair%%:*}"; src="${pair#*:}"; dst="$JNILIBS/$abi/libak_ffi.so"
  src_hash="$(sha256sum "$src" | awk '{print $1}')"
  dst_hash="$(sha256sum "$dst" | awk '{print $1}')"
  [ "$src_hash" = "$dst_hash" ] || die "jniLibs copy mismatch for $abi"
  printf '==> %-9s libak_ffi.so  %10s bytes  sha256 %s\n' \
    "$abi" "$(stat -c %s "$src")" "$src_hash"
done
echo "==> staged into: $JNILIBS/{x86_64,arm64-v8a}/libak_ffi.so"
echo "OK: android FFI build complete."
