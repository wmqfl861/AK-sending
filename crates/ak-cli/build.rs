//! Build-time source revision stamp (CR-P2-09, review 2026-11-11).
//!
//! `ak-cli --version` (and the registry-server startup log) must answer
//! with the exact source revision the binary was built from, so "what is
//! running on the production server?" stops being a guess. This script
//! stamps the short git commit — suffixed `-dirty` when the working tree
//! differs from HEAD (tracked changes, staged or unstaged; untracked files
//! do not count, matching `git describe --dirty`) — into the compile-time
//! `AK_GIT_SHA` variable consumed by `main.rs`.
//!
//! Hermetic by contract: a git-less source export must still build.
//! - an existing `AK_GIT_SHA` environment variable wins verbatim, so CI
//!   can inject the revision without git on the build machine;
//! - otherwise `git rev-parse --short HEAD` plus a `git diff --quiet HEAD`
//!   dirty probe, run from the workspace root;
//! - any failure (no git on PATH, no `.git`, unborn HEAD, non-hex output)
//!   degrades to `unknown` instead of failing the build.

use std::env;
use std::process::Command;

/// Marker stamped when the revision cannot be determined.
const UNKNOWN: &str = "unknown";

fn main() {
    // CI injection must re-stamp when the variable changes.
    println!("cargo:rerun-if-env-changed=AK_GIT_SHA");
    watch_fresh_revisions();
    let revision = stamped_revision();
    println!("cargo:rustc-env=AK_GIT_SHA={revision}");
}

/// Keep the stamp fresh across commits that touch no file of this package:
/// rerun when this script changes, when the checked-out branch changes
/// (HEAD), when a commit moves the current branch tip (.git/refs is watched
/// recursively), and when staging flips the dirty suffix (.git/index).
/// Watch lines are emitted only for paths that exist — an explicit
/// rerun-if-changed for a missing path warns on every git-less build, and
/// worktrees/submodules (where .git is a file) simply keep the build.rs
/// watch plus cargo's package staleness.
fn watch_fresh_revisions() {
    println!("cargo:rerun-if-changed=build.rs");
    for rel in ["../../.git/HEAD", "../../.git/refs", "../../.git/index"] {
        if env::current_dir()
            .map(|dir| dir.join(rel).exists())
            .unwrap_or(false)
        {
            println!("cargo:rerun-if-changed={rel}");
        }
    }
}

/// The revision string to stamp, per the module contract above.
fn stamped_revision() -> String {
    if let Some(injected) = env::var_os("AK_GIT_SHA") {
        // CI injection: use verbatim (possibly empty → unknown).
        let injected = injected.to_string_lossy().into_owned();
        return if injected.is_empty() {
            UNKNOWN.to_owned()
        } else {
            injected
        };
    }
    match git_short_sha() {
        Some(sha) if is_dirty() => format!("{sha}-dirty"),
        Some(sha) => sha,
        None => UNKNOWN.to_owned(),
    }
}

/// `git rev-parse --short HEAD` from the workspace root, accepted only
/// when it is pure hex (defensive: never stamp arbitrary command output
/// into the environment).
fn git_short_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let hex = !sha.is_empty() && sha.bytes().all(|b| b.is_ascii_hexdigit());
    hex.then_some(sha)
}

/// `git diff --quiet HEAD` exits nonzero exactly when tracked files differ
/// from HEAD (staged or unstaged; untracked files do not count).
fn is_dirty() -> bool {
    Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .status()
        .map(|status| !status.success())
        .unwrap_or(false)
}
