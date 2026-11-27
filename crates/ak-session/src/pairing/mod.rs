//! Automatic pairing orchestration (wave 21, task A6).
//!
//! This module tree binds the wave-20 deliverables into one end-to-end
//! pairing flow over the **real** v2 control plane:
//!
//! * [`crate::pairing::client`] — [`crate::pairing::RegistryClient`]: a real v2 control client (pinned
//!   TLS 1.3 over `ak-signal::server_v2::tls::connect_pinned`, the frozen
//!   `hello2` signature domains, the frozen frame codec) with a single
//!   reader task, bounded inboxes, idempotent request round-trips and
//!   reconnect. It speaks byte-exactly what the authoritative registry
//!   server verifies. (The wave-20 `ak_signal::client_v2::ClientV2` was
//!   developed in parallel against a scripted fake registry and diverged
//!   from the shipped server on three handshake constants — ALPN token,
//!   pin semantics, TLS-exporter label — so it cannot complete the opening
//!   exchange against `serve_v2`; the details are recorded in the module
//!   docs of [`crate::pairing::client`].)
//! * [`crate::pairing::transport`] — [`crate::pairing::RealTransport`]: the wave-21 real adapter that
//!   implements [`crate::runtime::transport::PairingTransport`] over
//!   [`crate::pairing::RegistryClient`] and plugs into
//!   [`crate::runtime::RuntimeConfig::transport`], the injection seam the
//!   wave-20 runtime left open.
//! * [`crate::pairing::orchestrator`] — [`crate::pairing::PairingOrchestrator`]: the owner/redeemer
//!   orchestration of the frozen pairing transcript
//!   (`InviteClaim` → `InviteRedeem` → offer → `PairChallenge` →
//!   `PairProof` → `PairPrepared` → `PairCommit` → local trust-book apply
//!   → `PairApplied` → `PairReady`), with the `ak-store` pairing journal
//!   for crash recovery and `ak-identity::PeerBookStore` for transactional
//!   `peers.book` writes.
//!
//! Everything here is endpoint-side: invite secrets and `auth_key` values
//! never leave this device, and every server-visible artefact is a fixed,
//! bounded, structured control message (ADR-001).
//!
//! ## Deviations from the wave-21 brief, and why
//!
//! * The brief sketches `create_invite` as
//!   `generate → journal.record_pending → claim`. The frozen
//!   `ak-store::PairingJournal` is **pairing-transaction** scoped: its
//!   identity key is the server-assigned `pairing_id`, which does not
//!   exist at claim time, and a placeholder key would conflate the several
//!   distinct transactions a long-lived code can produce. Journal entries
//!   are therefore recorded at the moment each pairing transaction comes
//!   into existence (redeem acknowledgment on the redeemer, redeem offer
//!   on the owner) — before any proof is exchanged, which is the crash
//!   window that matters.
//! * The brief sequences `send_pair_prepared` after `PairCommit`. The
//!   frozen server commits exactly when **both** `PairPrepared` frames
//!   have arrived, so the orchestrator sends its `PairPrepared` right
//!   after its proof and then awaits `PairCommit` (the only order the
//!   frozen state machine accepts).
#![forbid(unsafe_code)]

pub mod client;
/// One line of the pairing trace the T03 integration test asks for (Q-N3
/// recurrence, 2026-11-27). `AK_PAIR_DIAG` on the process turns it on, and
/// it goes to the `ak_pair_diag` log target, so nothing is written unless a
/// logger with that target enabled is installed as well — only that test
/// does both. A line names the side and the step and counts milliseconds
/// from the redeem deadline (negative is before it); it never carries a
/// code, a key or an id beyond the first four bytes of a pairing id, which
/// only tell one round from another. A failed flow is named by the class of
/// its failure, never by a message that could carry a path or a full id.
macro_rules! pair_diag {
    ($($arg:tt)*) => {
        if $crate::pairing::pair_diag_enabled() {
            log::info!(target: "ak_pair_diag", $($arg)*);
        }
    };
}
pub(crate) use pair_diag;

/// Whether the pairing trace is on: `AK_PAIR_DIAG` non-empty on the process
/// (read once) AND a logger that lets `ak_pair_diag` through at `info`.
/// Whatever a line would cost — a frame description, an id prefix, a place
/// in a reader's count — is built only behind this (review 21), so a
/// process without the trace neither writes nor builds any of it. The
/// logger is asked every time, not cached: one may be installed after the
/// first frame.
pub(crate) fn pair_diag_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("AK_PAIR_DIAG").is_some_and(|value| !value.is_empty()))
        && log::log_enabled!(target: "ak_pair_diag", log::Level::Info)
}

/// Test-only: how many id prefixes the trace built (the switch tests prove
/// that none is built while the trace is off).
#[cfg(test)]
pub(crate) static ID_PREFIXES_BUILT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// The first four bytes of an id as hex, for the pairing trace (review 20:
/// the registry's lines carry the same prefix).
pub(crate) fn id_prefix(id: &[u8]) -> String {
    #[cfg(test)]
    ID_PREFIXES_BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    id.iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Test support of the pairing trace (`#[cfg(test)]`): a logger that keeps
/// the `ak_pair_diag` lines in memory, and the re-exec that gives one test
/// a process of its own. The trace's switch has process-wide state — the
/// environment read once, the one logger per process — so a test that
/// changes either runs its body in a child copy of this test executable.
#[cfg(test)]
pub(crate) mod diag_test {
    use std::sync::{Arc, Mutex as StdMutex};

    /// The positions the switch has.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Switch {
        /// `AK_PAIR_DIAG` unset, under a logger that would show the trace.
        Off,
        /// `AK_PAIR_DIAG=1`, under a logger that shows `info` but keeps
        /// `ak_pair_diag` out (the strict half: the log crate's own level
        /// check lets the line through, only the target refuses it).
        LevelOff,
        /// `AK_PAIR_DIAG=1`, under a logger that shows `ak_pair_diag`.
        On,
    }

    /// The switch a child process runs under, and the lines it captured.
    pub(crate) struct Traced {
        pub(crate) switch: Switch,
        lines: Arc<StdMutex<Vec<String>>>,
        others: Arc<StdMutex<Vec<String>>>,
    }

    impl Traced {
        /// Every `ak_pair_diag` line written so far, in order.
        pub(crate) fn lines(&self) -> Vec<String> {
            self.lines.lock().expect("captured lines lock").clone()
        }

        /// What the registry and the clients themselves said at `info`, in
        /// order: when a pairing stops on its way, their decision is here.
        pub(crate) fn others(&self) -> Vec<String> {
            self.others.lock().expect("captured lines lock").clone()
        }

        /// How many lines contain `text`.
        pub(crate) fn count(&self, text: &str) -> usize {
            self.lines()
                .iter()
                .filter(|line| line.contains(text))
                .count()
        }

        /// The one line that contains `text` (panics unless exactly one).
        pub(crate) fn the_one(&self, text: &str) -> String {
            let found: Vec<String> = self
                .lines()
                .into_iter()
                .filter(|line| line.contains(text))
                .collect();
            assert!(
                found.len() == 1,
                "expected exactly one line with {text:?}, found {}:\n{}",
                found.len(),
                self.lines().join("\n")
            );
            found[0].clone()
        }

        /// Whether a line containing `first` comes before a line containing
        /// `second`.
        pub(crate) fn before(&self, first: &str, second: &str) -> bool {
            let lines = self.lines();
            let at = |text: &str| {
                lines
                    .iter()
                    .position(|line| line.contains(text))
                    .unwrap_or_else(|| {
                        panic!("no line with {text:?}; lines:\n{}", lines.join("\n"))
                    })
            };
            at(first) < at(second)
        }
    }

    /// Names the child run of a test and the switch it was started under.
    const CHILD_VAR: &str = "AK_SESSION_PAIR_DIAG_CHILD";

    /// Keeps what it is shown: the lines of `ak_pair_diag` apart from what
    /// every other target says at `info` (the registry server's decisions,
    /// the client's warnings).
    #[derive(Clone)]
    struct Capturing {
        lines: Arc<StdMutex<Vec<String>>>,
        others: Arc<StdMutex<Vec<String>>>,
        show_trace: bool,
    }

    impl log::Log for Capturing {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
                && (metadata.target() != "ak_pair_diag" || self.show_trace)
        }

        fn log(&self, record: &log::Record) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let kept = if record.target() == "ak_pair_diag" {
                &self.lines
            } else {
                &self.others
            };
            kept.lock()
                .expect("captured lines lock")
                .push(record.args().to_string());
        }

        fn flush(&self) {}
    }

    /// In the parent (no child variable): run this very test again in a
    /// child process under `switch` and require that exactly that one test
    /// passed there; `None`. In the child: install the logger of `switch`
    /// and hand back the lines it captures; `Some`.
    ///
    /// A wrong `test` name cannot pass by running nothing: the child's own
    /// summary must say `1 passed`.
    pub(crate) fn traced_process(test: &str, switch: Switch) -> Option<Traced> {
        let name = match switch {
            Switch::Off => "off",
            Switch::LevelOff => "level-off",
            Switch::On => "on",
        };
        let Some(child) = std::env::var_os(CHILD_VAR) else {
            let mut command =
                std::process::Command::new(std::env::current_exe().expect("this test executable"));
            command.args(["--exact", test, "--nocapture", "--test-threads=1"]);
            command.env(CHILD_VAR, name);
            match switch {
                Switch::Off => {
                    command.env_remove("AK_PAIR_DIAG");
                }
                _ => {
                    command.env("AK_PAIR_DIAG", "1");
                }
            }
            let output = command.output().expect("start the child test process");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "the child run of `{test}` under `{name}` did not pass ({})\n--- child stdout ---\n{stdout}\n--- child stderr ---\n{stderr}",
                output.status
            );
            return None;
        };
        assert_eq!(
            child,
            std::ffi::OsString::from(name),
            "the child was started under another switch"
        );
        let lines = Arc::new(StdMutex::new(Vec::new()));
        let others = Arc::new(StdMutex::new(Vec::new()));
        // Whatever fails in this process fails with its evidence: after the
        // panic message, what the registry and the clients said. A pairing
        // that stops on its way shows the registry's decision right there
        // (it refuses a signature by disconnecting, or by answering
        // nothing), which the assertion alone does not.
        let (trace, said) = (Arc::clone(&lines), Arc::clone(&others));
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            let show = |kept: &StdMutex<Vec<String>>, what: &str| {
                let kept = kept.lock().expect("captured lines lock").join("\n");
                eprintln!("--- {what} ---\n{kept}");
            };
            show(&trace, "the pairing trace so far");
            show(&said, "what the registry and the clients said at info");
        }));
        log::set_boxed_logger(Box::new(Capturing {
            lines: Arc::clone(&lines),
            others: Arc::clone(&others),
            show_trace: switch != Switch::LevelOff,
        }))
        .expect("no logger is installed in a child process before its test");
        // The level the crate checks first: at `info` even for `LevelOff`,
        // so that case is refused by the target alone.
        log::set_max_level(log::LevelFilter::Info);
        Some(Traced {
            switch,
            lines,
            others,
        })
    }

    /// [`traced_process`] for each position of the switch in turn. In the
    /// parent it runs the three children and returns `None`; in a child it
    /// returns that child's switch and lines.
    pub(crate) fn every_switch(test: &str) -> Option<Traced> {
        let child = std::env::var_os(CHILD_VAR);
        if child.is_none() {
            for switch in [Switch::Off, Switch::LevelOff, Switch::On] {
                assert!(traced_process(test, switch).is_none());
            }
            return None;
        }
        let switch = match child.as_deref().and_then(|value| value.to_str()) {
            Some("off") => Switch::Off,
            Some("level-off") => Switch::LevelOff,
            Some("on") => Switch::On,
            _ => panic!("unknown child switch {child:?}"),
        };
        traced_process(test, switch)
    }
}

/// The per-identity registry control-connection lock shared by the CLI
/// task commands, the daemon and the FFI task runtime (R36-P2-04, batch
/// G6 — see the module docs).
pub mod connection_lock;
pub mod error;
pub mod orchestrator;
pub mod task;
/// The v1 task-plane connectivity plumbing shared by ak-ffi and ak-cli
/// (CR-P2-11 dedup — see the module docs). Deliberately NOT re-exported
/// wholesale at the `pairing` root: the adapters import it as
/// `pairing::task_net::{…}` so the shared surface stays greppable.
pub mod task_net;
pub mod transport;

pub use ak_identity::pairing_txn::VerifiedPairing;
pub use client::{ClientConfig, ClientState, RegistryClient};
pub use connection_lock::{
    acquire_connection_lock, spawn_connection_lock_heartbeat, ConnectionLock, ConnectionLockError,
    ConnectionLockLease, CONNECTION_LOCK_FILE, CONNECTION_LOCK_REFRESH, CONNECTION_LOCK_STALE,
};
pub use error::{ClientError, PairFlowError};
pub use orchestrator::{
    redeem_request_mac, CommittedPairing, CreatedInvite, OrchestratorConfig, PairingOrchestrator,
    PersistedCodeSource, RedeemOffer, RedeemOfferGate, TaskRedeem, BOOK_NAME, JOURNAL_SUBDIR,
};
pub use task::{
    decode_endpoint_blob, encode_endpoint_blob, order_candidates_lan_first, probe_peer, ActiveTask,
    CandidateFailure, PeerEndpoint, ProbeOptions, ProbeOutcome, TaskCode, TaskDirection,
    TaskDisconnectReason, TaskError, TaskEvent, TaskPaired, TaskPairing, TaskPairingConfig,
    ENDPOINT_BLOB_MAX_BYTES, ENDPOINT_BLOB_MAX_CANDIDATES, TASK_CODE_TTL_SECS,
};
pub use transport::RealTransport;
