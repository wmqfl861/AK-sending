//! Single-identity resident runtime (wave 20, B6 — Fake-DI form).
//!
//! This module wraps the wave-19 [`SchedulerCore`](crate::scheduler::SchedulerCore)
//! in an actor and orchestrates the task lifecycle of contract
//! `docs/contracts/scheduler.md` §5–§7:
//!
//! * [`actor`] — the scheduler actor: one task serially owning the core
//!   (B0 integration point 1), a bounded command channel
//!   (`Submit`/`Cancel`/`SetPolicy`/`Retry`/`Snapshot`/`Shutdown` plus
//!   executor reports and backoff timers), a bounded event broadcast
//!   (1024, drop-oldest for slow consumers) and an admission pump after
//!   every command;
//! * task lifecycle — submit → queue → admission pump → pairing-driver →
//!   `TransferExecutor::enqueue` → phase reports into
//!   `SchedulerCore::report_phase` → finish into `SchedulerCore::finish`;
//!   failures back off on the injected clock, then requeue or park
//!   (retry budget exhausted); cancels remove queued items directly and
//!   stop running ones through the executor (contract §10);
//! * [`listener`] — [`RuntimeListener`]: resident consumption of pairing
//!   events; every completed pairing submits an inbound receive task into
//!   the same scheduler (explicit stop + join);
//! * shutdown — stop new admissions, wait in-flight (bounded by
//!   `shutdown_timeout_ms` on the injected clock), cancel residuals,
//!   return a [`ShutdownSummary`] and join actor + listener;
//! * [`handle`] — the public seven-signature API of contract §4
//!   (`SessionRuntime::start` + `SessionHandle::{enqueue, set_policy,
//!   cancel, retry, snapshot, shutdown}`).
//!
//! **Fake-DI form (wave 20):** both real-world dependencies are injected
//! traits — [`PairingTransport`] (signaling/pairing, method families
//! mirroring the A3 control plane: claim/redeem/pair/open-session/events)
//! and [`TransferExecutor`] (engine: enqueue one task, report
//! phase/finish). This crate ships deterministic fakes
//! ([`FakePairingTransport`], [`FakeTransferExecutor`], [`FakeClock`])
//! plus a production [`RealClock`]; wave 21 binds the real `ak-signal`
//! client, the B2/B3 admission handshake and the `ak-engine` on-channel
//! executor behind the same traits.
//!
//! No networking, no file I/O, no wall clock — everything observable is
//! driven through the injected ports.
pub mod actor;
pub mod clock;
pub mod error;
pub mod executor;
pub mod handle;
pub mod listener;
pub mod real_executor;
pub mod transport;
pub mod types;

pub use clock::{FakeClock, RealClock, RuntimeClock};
pub use error::{EngineStartError, SessionError};
pub use executor::{
    ExecutorReport, FakeScript, FakeTransferExecutor, ProgressSink, TaskInterval, TransferExecutor,
};
pub use handle::{SessionHandle, SessionRuntime};
pub use listener::{ListenerStats, RuntimeListener};
pub use real_executor::{RealFailure, RealOutcome, RealTransferExecutor};
pub use transport::{
    FakePairingTransport, InviteClaimRequest, InviteClaimed, OpenSessionInfo, PairingConfirmation,
    PairingError, PairingEvent, PairingFuture, PairingId, PairingReadyInfo, PairingTransport,
    PeerFingerprint, RedeemAccepted, RedeemPayload,
};
pub use types::{
    CancelReceipt, ListenerView, PairingPlan, ParkReason, RetryPolicy, RuntimeConfig, SessionEvent,
    SessionSnapshot, ShutdownMode, ShutdownSummary, SnapshotQuery, TaskView, TransferDirection,
    TransferId, TransferSpec, EVENT_CAPACITY,
};

// Re-export the scheduler-domain vocabulary runtime callers need, so the
// FFI/CLI layers can `use ak_session::runtime::*` alone.
pub use crate::scheduler::config::SchedulerPolicy;
pub use crate::scheduler::core::PolicySwitch;
pub use crate::scheduler::key::{CodeGroupKey, CodeId, RegistryId, TaskId};
pub use crate::scheduler::state::{FinishOutcome, TaskPhase, TaskState};
