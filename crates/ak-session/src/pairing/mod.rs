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
pub mod error;
pub mod orchestrator;
pub mod task;
pub mod transport;

pub use ak_identity::pairing_txn::VerifiedPairing;
pub use client::{ClientConfig, ClientState, RegistryClient};
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
