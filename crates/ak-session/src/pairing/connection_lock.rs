//! The per-identity registry control-connection lock (R36-P2-04), shared
//! by EVERY local participant of one identity's registry connection: the
//! CLI task commands, the resident daemon, and the FFI task runtime
//! (batch G6 — the client half of R36-P2-04; the GUI embeds the same Rust
//! runtime, so the lock is enforced on the Rust side, not in
//! `flutter_app`).
//!
//! Why the lock exists: the v2 registry keeps exactly ONE live connection
//! per device id and REPLACES an older same-device connection on a new
//! Hello. Two local processes (or two task runtimes) sharing one `id_dir`
//! would therefore silently kick each other's control connection forever.
//! The lock makes the second participant refuse loudly instead.
//!
//! Protocol (best-effort by design, a cooperative local file — it cannot
//! stop a copied key on another machine, and the server-side replacement
//! reason frame of 9029c08 remains the cross-machine backstop):
//!
//! * `id_dir/registry-connection.lock` holds `"<pid>\n<heartbeat-unix-ms>\n"`.
//! * While holding, the owner refreshes the heartbeat every
//!   [`CONNECTION_LOCK_REFRESH`]; a heartbeat older than
//!   [`CONNECTION_LOCK_STALE`] means the holder crashed or suspended and
//!   the lock is breakable by the next run.
//! * Wave-36 audit N2 hardening (batch G6):
//!   - acquisition is a **create-new exclusive claim**, then a read-check
//!     loop — two concurrent acquirers can no longer both pass a
//!     read-check-write window and both write (the loser of the create
//!     re-checks and refuses against the fresh winner);
//!   - a FRESH lock naming OUR OWN pid refuses too: the registry allows
//!     one connection per device, so a second runtime in the same process
//!     (two FFI task runtimes share one PID) must not silently stack;
//!   - the heartbeat refreshes ONLY while the file still names this
//!     process: a holder suspended past the stale window, broken out of
//!     by the next run, stops stamping on wake instead of overwriting
//!     the new holder (the original N2 defect);
//!   - a write failure in the heartbeat is logged, never ignored.
//!
//! One-shot registry operations (e.g. `ak-cli invite revoke`) deliberately
//! do NOT participate in this lock — a one-shot against a live daemon is a
//! supported path that relies on the server's replacement reason frame and
//! the keeper's replacement pacing, not on local exclusion. Do not make
//! them lock participants.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::client::now_unix_ms;

/// Lock file under `id_dir` marking which local process holds this
/// identity's registry control connection (`pid` + heartbeat `unix-ms`).
pub const CONNECTION_LOCK_FILE: &str = "registry-connection.lock";
/// While holding, the session refreshes the heartbeat at this cadence.
pub const CONNECTION_LOCK_REFRESH: Duration = Duration::from_secs(5);
/// A heartbeat older than this means the holder crashed (or suspended):
/// the lock is breakable by the next run. Generous versus the refresh
/// cadence, short versus a user's patience.
pub const CONNECTION_LOCK_STALE: Duration = Duration::from_secs(15);
/// Bounded attempts for the create→check→(break)→create loop: a lock that
/// keeps changing hands under concurrent breakers fails loudly instead of
/// spinning.
const ACQUIRE_ATTEMPTS: usize = 4;

/// One parsed lock file (`pid heartbeat-ms`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockHolder {
    pub pid: u32,
    pub beat_ms: u64,
}

pub fn parse_lock_holder(text: &str) -> Option<LockHolder> {
    let mut parts = text.split_whitespace();
    let pid = parts.next()?.parse().ok()?;
    let beat_ms = parts.next()?.parse().ok()?;
    Some(LockHolder { pid, beat_ms })
}

fn read_holder(path: &Path) -> Option<LockHolder> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse_lock_holder(&text))
}

/// The next heartbeat stamp of THIS process.
fn lock_line() -> String {
    format!("{}\n{}\n", std::process::id(), now_unix_ms())
}

/// Atomically CLAIM the lock name: `create_new` fails with `AlreadyExists`
/// when any file holds it, which is exactly the mutual exclusion two
/// concurrent acquirers need (N2: the old read-check-write acquisition let
/// both pass the check).
fn try_create_exclusive(path: &Path) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    // A create that fails mid-write leaves an unparseable file — the next
    // acquirer treats that as breakable (best-effort, documented).
    file.write_all(lock_line().as_bytes())
}

/// Why [`acquire_connection_lock`] refused.
#[derive(Debug)]
pub enum ConnectionLockError {
    /// A live holder (fresh heartbeat) in ANOTHER process owns the lock.
    HeldByOtherProcess { pid: u32 },
    /// A fresh lock names THIS process: a second runtime in the same
    /// process already holds it (two FFI task runtimes share one PID, and
    /// the registry allows one connection per device id).
    HeldByThisProcess,
    /// The lock could not be claimed after bounded retries (I/O error or
    /// persistent contention between concurrent breakers).
    NotAcquired(String),
}

impl std::fmt::Display for ConnectionLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionLockError::HeldByOtherProcess { pid } => write!(
                f,
                "this identity is already holding a control connection \
                 (process {pid})"
            ),
            ConnectionLockError::HeldByThisProcess => write!(
                f,
                "this process already holds this identity's control \
                 connection (a second runtime is not supported)"
            ),
            ConnectionLockError::NotAcquired(reason) => write!(
                f,
                "cannot claim the identity's control-connection lock: {reason}"
            ),
        }
    }
}

impl std::error::Error for ConnectionLockError {}

/// The run's hold on the identity's control connection: acquired before
/// the registry dial, released (file removed) on drop — including every
/// failure path of the caller. The removal only happens while the file
/// still names THIS process, so a run whose stale lock was broken out
/// from under it cannot unlock the new holder.
#[derive(Debug)]
pub struct ConnectionLock {
    path: PathBuf,
}

/// Claim the identity's control connection for THIS process
/// ([`CONNECTION_LOCK_FILE`]). A live holder in another process (fresh
/// heartbeat) refuses loudly — the registry would otherwise let the two
/// participants silently kick each other's connection forever. A fresh
/// lock naming OUR OWN pid also refuses (same-process second runtime).
/// A stale lock (crashed/suspended holder, heartbeat past
/// [`CONNECTION_LOCK_STALE`]) or an unparseable one is broken: the break
/// is a remove followed by another exclusive create, so a concurrent
/// breaker that wins the re-create makes THIS acquire refuse against its
/// fresh file instead of overwriting it.
pub fn acquire_connection_lock(id_dir: &Path) -> Result<ConnectionLock, ConnectionLockError> {
    let path = id_dir.join(CONNECTION_LOCK_FILE);
    for _attempt in 0..ACQUIRE_ATTEMPTS {
        match try_create_exclusive(&path) {
            Ok(()) => return Ok(ConnectionLock { path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match read_holder(&path) {
                    None => {
                        // Unparseable (corrupt / torn write): breakable.
                        let _ = std::fs::remove_file(&path);
                    }
                    Some(holder) => {
                        let age_ms = now_unix_ms().saturating_sub(holder.beat_ms);
                        let fresh = age_ms < CONNECTION_LOCK_STALE.as_millis() as u64;
                        if fresh && holder.pid != std::process::id() {
                            return Err(ConnectionLockError::HeldByOtherProcess {
                                pid: holder.pid,
                            });
                        }
                        if fresh {
                            return Err(ConnectionLockError::HeldByThisProcess);
                        }
                        // Crashed or suspended holder: break the stale file.
                        // Losing the re-create to another breaker is handled
                        // by the next iteration's fresh-holder refusal.
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
            Err(e) => return Err(ConnectionLockError::NotAcquired(e.to_string())),
        }
    }
    Err(ConnectionLockError::NotAcquired(
        "the lock kept changing hands while acquiring".to_string(),
    ))
}

/// Best-effort heartbeat refresh. `true` when the file still names THIS
/// process (so the caller keeps refreshing); `false` when the hold was
/// broken out from under us — the caller MUST stop refreshing: the file
/// belongs to whichever run broke it now (N2: the old heartbeat never
/// looked and re-stamped a foreign lock every 5 s).
fn refresh_if_held(path: &Path) -> bool {
    match read_holder(path) {
        Some(holder) if holder.pid == std::process::id() => {
            if let Err(e) = std::fs::write(path, lock_line()) {
                log::debug!("connection lock heartbeat write failed: {e}");
            }
            true
        }
        _ => false,
    }
}

/// Keep the heartbeat fresh while the session lives, so a later run on
/// the same id-dir sees a LIVE holder (a crashed process stops refreshing
/// and its lock goes stale). The task exits as soon as the file no longer
/// names this process — drop the returned handle to detach, or abort it
/// (the [`ConnectionLockLease`] wrapper does that on drop).
pub fn spawn_connection_lock_heartbeat(lock: &ConnectionLock) -> tokio::task::JoinHandle<()> {
    let path = lock.path.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CONNECTION_LOCK_REFRESH).await;
            if !refresh_if_held(&path) {
                return;
            }
        }
    })
}

impl Drop for ConnectionLock {
    fn drop(&mut self) {
        // Remove the file only when it still names THIS process: a run
        // that broke our stale lock (we suspended past the stale window)
        // owns it now and must not be unlocked by our exit.
        if let Some(holder) = read_holder(&self.path) {
            if holder.pid == std::process::id() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// A guard plus its heartbeat task, for holders whose lifetime is not a
/// single function body — the daemon keeps it for the whole run, the FFI
/// task stack keeps it until teardown. Dropping the lease first ABORTS
/// the heartbeat, then releases the file: a merely-detached heartbeat
/// would keep stamping the lock after the guard is gone (the daemon's
/// original lifecycle bug — the guard died at the end of the block that
/// built it while the shared client lived on).
pub struct ConnectionLockLease {
    lock: Option<ConnectionLock>,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl ConnectionLockLease {
    /// [`acquire_connection_lock`] plus a heartbeat task, in one value
    /// that releases both on drop.
    pub fn acquire(id_dir: &Path) -> Result<Self, ConnectionLockError> {
        let lock = acquire_connection_lock(id_dir)?;
        let heartbeat = spawn_connection_lock_heartbeat(&lock);
        Ok(ConnectionLockLease {
            lock: Some(lock),
            heartbeat: Some(heartbeat),
        })
    }
}

impl Drop for ConnectionLockLease {
    fn drop(&mut self) {
        if let Some(task) = self.heartbeat.take() {
            task.abort();
        }
        drop(self.lock.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_id_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp id dir")
    }

    fn write_holder(path: &Path, pid: u32, age: Duration) {
        let beat = now_unix_ms().saturating_sub(age.as_millis() as u64);
        std::fs::write(path, format!("{pid}\n{beat}\n")).expect("write holder lock");
    }

    fn lock_path(id_dir: &Path) -> PathBuf {
        id_dir.join(CONNECTION_LOCK_FILE)
    }

    #[test]
    fn live_foreign_holder_refuses() {
        let id_dir = temp_id_dir();
        write_holder(&lock_path(id_dir.path()), 424_242, Duration::from_secs(1));
        match acquire_connection_lock(id_dir.path()) {
            Err(ConnectionLockError::HeldByOtherProcess { pid: 424_242 }) => {}
            other => panic!("a live foreign holder must refuse: {other:?}"),
        }
        // The refusal names the holder in its Display (CLI surfaces it).
        let text = ConnectionLockError::HeldByOtherProcess { pid: 424_242 }.to_string();
        assert!(
            text.contains("424242"),
            "the error names the holding process"
        );
    }

    #[test]
    fn fresh_own_pid_refuses_a_second_runtime() {
        let id_dir = temp_id_dir();
        write_holder(
            &lock_path(id_dir.path()),
            std::process::id(),
            Duration::ZERO,
        );
        // N2/G6: the same PID is no longer "ours to break" — two FFI task
        // runtimes share one PID and the registry allows one connection
        // per device, so the second runtime refuses loudly.
        match acquire_connection_lock(id_dir.path()) {
            Err(ConnectionLockError::HeldByThisProcess) => {}
            other => panic!("a fresh own-pid lock must refuse: {other:?}"),
        }
    }

    #[test]
    fn stale_lock_is_broken_exclusively_and_released_on_drop() {
        let id_dir = temp_id_dir();
        write_holder(
            &lock_path(id_dir.path()),
            424_242,
            CONNECTION_LOCK_STALE * 4,
        );
        let lock = acquire_connection_lock(id_dir.path()).expect("stale lock is breakable");
        let text = std::fs::read_to_string(lock_path(id_dir.path())).unwrap();
        assert!(
            text.starts_with(&format!("{}\n", std::process::id())),
            "the broken lock now names THIS process: {text:?}"
        );
        drop(lock);
        assert!(
            !lock_path(id_dir.path()).exists(),
            "drop removes the lock we own"
        );

        // Sequential runs in one process still work after a proper drop:
        // the file is gone, so the next acquire creates fresh.
        let again = acquire_connection_lock(id_dir.path()).expect("re-acquire after release");
        drop(again);
        assert!(!lock_path(id_dir.path()).exists());
    }

    #[test]
    fn unparseable_lock_is_breakable() {
        let id_dir = temp_id_dir();
        std::fs::write(lock_path(id_dir.path()), "not a lock").unwrap();
        let lock = acquire_connection_lock(id_dir.path()).expect("corrupt lock is breakable");
        drop(lock);
        assert!(!lock_path(id_dir.path()).exists());
    }

    #[test]
    fn stale_own_pid_lock_is_breakable_after_a_crashed_runtime() {
        // A previous runtime of THIS process crashed without Drop (or the
        // PID was reused): its heartbeat is far past the stale window, so
        // the lock is breakable even though it names our pid.
        let id_dir = temp_id_dir();
        write_holder(
            &lock_path(id_dir.path()),
            std::process::id(),
            CONNECTION_LOCK_STALE * 4,
        );
        let lock = acquire_connection_lock(id_dir.path()).expect("own stale lock is breakable");
        drop(lock);
        assert!(!lock_path(id_dir.path()).exists());
    }

    #[test]
    fn heartbeat_stops_refreshing_once_broken_out() {
        // N2's defect scenario: holder A is suspended past the stale
        // window, run B breaks the lock and takes over; when A's
        // heartbeat wakes it must NOT stamp B's file back to A's pid.
        let id_dir = temp_id_dir();
        let lock = acquire_connection_lock(id_dir.path()).expect("acquire");
        let path = lock_path(id_dir.path());
        // B breaks us out (fresh foreign file).
        write_holder(&path, 424_243, Duration::ZERO);
        assert!(
            !refresh_if_held(&path),
            "the heartbeat sees the hold is gone"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("424243\n"),
            "the breaker's file is untouched: {text:?}"
        );
        // Our guard's Drop must not remove B's file either.
        drop(lock);
        assert!(path.exists(), "drop leaves the new holder's file alone");
    }

    #[test]
    fn heartbeat_refreshes_a_live_own_hold() {
        let id_dir = temp_id_dir();
        let lock = acquire_connection_lock(id_dir.path()).expect("acquire");
        let path = lock_path(id_dir.path());
        assert!(refresh_if_held(&path), "still ours");
        let holder = parse_lock_holder(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(holder.pid, std::process::id());
        drop(lock);
        assert!(!path.exists());
    }
}
