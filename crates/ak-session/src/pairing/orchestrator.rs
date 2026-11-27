//! Automatic pairing orchestration: the owner/redeemer state machines of
//! the frozen pairing transcript (wave 21, task A6).
//!
//! One [`PairingOrchestrator`] per device, sharing the device's single
//! [`RegistryClient`] with the runtime transport. It owns three local
//! artefacts under an identity directory:
//!
//! * `pairing/` — the `ak-store` [`PairingJournal`] (one `.akpair` file per
//!   pairing transaction, `pending → committed → applied`);
//! * `peers.book` (+ `.pairings` sidecar) — the transactional trust book
//!   written through `ak-identity`'s [`PeerBookStore`];
//! * an in-memory invite-secret registry — `secret_ref → InviteCode`. The
//!   plaintext code and its `auth_key` never touch disk (B.6.4): a journal
//!   entry whose `secret_ref` is no longer resident cannot produce proofs
//!   until the operator re-supplies the code (see [`PairingOrchestrator::recover`]).
//!
//! ## Redeemer path ([`PairingOrchestrator::redeem`])
//!
//! ```text
//! parse code → RedeemRequest (nonce, deadline, MAC(auth_key), signature)
//!   → InviteRedeem → InviteRedeemPending{pairing_id, deadline}
//!   → (server offers the redeem to the owner)
//!   ← PairChallenge{transcript, owner_proof}   [verified: MAC + signature]
//!   → journal.record_pending → PairProof{redeemer_proof} → PairPrepared
//!   ← PairCommit{transcript, both proofs, revision}  [fully verified]
//!   → peers.book commit → journal.mark_committed → PairApplied
//!   ← PairReady → journal.mark_applied → VerifiedPairing
//! ```
//!
//! ## Owner path ([`PairingOrchestrator::spawn_owner_loop`])
//!
//! The owner passively consumes the client's pairing plane. On
//! `InviteRedeemOffered` it verifies the redeemer's request MAC (proof of
//! invite-secret knowledge — the digest alone is not enough) and Ed25519
//! signature, records the pending transaction, builds the transcript with
//! a fresh owner nonce, and drives the same
//! challenge → proof → prepared → commit → apply → ready sequence.
//!
//! Before any of that, an optional **redeem-offer gate**
//! ([`PairingOrchestrator::set_redeem_offer_gate`], wave 29 / review
//! 2026-11-06 P1-05) inspects the offer — including the redeemer's
//! wave-28 endpoint blob, peeked without consuming it. A refusing gate
//! aborts the transaction instead of answering it: the server never
//! receives the owner's `PairChallenge`/`PairPrepared`, so it never
//! commits (it commits exactly when both `PairPrepared` frames have
//! arrived), the aborted attempt does not consume the code's redeem
//! budget, the redeemer observes the unified `PAIR_UNAVAILABLE` before
//! any of its local writes, and neither side journals or books the
//! pairing. This is the pre-commit point task-level rules (direction
//! complement, endpoint presence) are enforced at.
//!
//! ## Recovery ([`PairingOrchestrator::recover`])
//!
//! On startup, `journal.load_all()` surfaces every transaction that did
//! not reach `applied`. For each, a participants-only `PairStatusQuery`
//! learns the authoritative state, and the server's fixed re-sends (the
//! `PairCommit`/`PairReady` that follow the status reply) let the local
//! side finish idempotently. Codes re-supplied via `code_hints` restore
//! full MAC verification. Without the secret, recovery trusts only a
//! LOCAL pin: every transcript this device authenticated leaves a
//! `<pairing hex>.akdgm` sidecar in the journal directory (written
//! before the `pending` journal entry, so an entry always implies a
//! pin), and the re-sent `PairCommit` must reproduce that pinned
//! transcript byte-for-byte (BLAKE3 digest equality over the 256-byte
//! canonical transcript) plus both Ed25519 signatures under the keys the
//! pinned transcript names — a malicious registry that swaps device
//! keys and re-signs cannot satisfy the pin. With neither a code hint
//! nor a pinned digest the entry is refused outright: the server's
//! self-reported `Committed`/`Ready` state is never the only evidence
//! (review 2026-11-05, P1-1).
//!
//! ## Abort policy
//!
//! A failed owner flow sends `PairAbort(Protocol)` so the server tombstones
//! the transaction; redeem failures surface the server's unified
//! `PAIR_UNAVAILABLE` unchanged.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use crate::pairing::id_prefix;

use ak_identity::invite::InviteCode;
use ak_identity::pairing_txn::PairingVerifyError::{InviteCodeMismatch, PublicKeyMismatch};
use ak_identity::pairing_txn::{
    random_id16, random_nonce32, PairProofData, PairTranscript, PairingRecord, VerifiedPairing,
    VerifyExpectations, OWNER_PROOF_DOMAIN, REDEEMER_PROOF_DOMAIN,
};
use ak_identity::peer_store::PeerBookStore;
use ak_identity::{DeviceKeyPair, Fingerprint, PeerRecord};
use ak_signal::client_v2::RevokedInvite;
use ak_signal::protocol_v2::message::CODE_CONFLICT;
use ak_signal::protocol_v2::{AbortReason, CtrlMessage, PairProof as WireProof, PairingState};
use ak_signal::server_v2::hello2;
use ak_store::{PairingJournal, PairingPhase, PendingPairing};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::pairing::client::{now_unix_ms, RegistryClient};
use crate::pairing::error::{ClientError, PairFlowError};

/// Subdirectory of the identity dir holding the pairing journal.
pub const JOURNAL_SUBDIR: &str = "pairing";
/// Trust-book file name inside the identity dir.
pub const BOOK_NAME: &str = "peers.book";

/// Domain of the redeem request MAC (mirrors the frozen signature domain
/// of `hello2::REDEEM_DOMAIN`; the MAC is endpoint-only material the
/// server never verifies).
const REDEEM_MAC_DOMAIN: &[u8; 20] = b"AK redeem request v1";

/// Tunables of the orchestration.
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Redeem window offered to the server (must be > 0 and ≤ 120 000,
    /// the server's `REDEEM_MAX_WINDOW`). Tests lower it to make the
    /// unified deadline failures fast.
    pub redeem_window_ms: u64,
    /// How long a finished-commit side waits for `PairReady` (the other
    /// side's apply) before reporting `Timeout`.
    pub ready_wait_ms: u64,
    /// Regeneration attempts of `create_invite` on `CODE_CONFLICT`.
    pub claim_conflict_retries: u32,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        OrchestratorConfig {
            redeem_window_ms: 90_000,
            ready_wait_ms: 60_000,
            claim_conflict_retries: 8,
        }
    }
}

/// A freshly registered invite code, ready to display.
///
/// `Debug` is hand-written (review 2026-11-05, P2): `display` is the
/// bearer credential and must never reach logs — only the masked
/// `hint` and the non-secret metadata are printed.
#[derive(Clone, PartialEq, Eq)]
pub struct CreatedInvite {
    /// Canonical display form (`AKI1-XXXX-…`); the bearer credential.
    pub display: String,
    /// Masked form for logs and lists.
    pub hint: String,
    /// Server-assigned code identity.
    pub code_id: [u8; 16],
    /// 0 = long-lived.
    pub expires_ms: u64,
    /// Registry revision of the code.
    pub revision: u64,
    /// The server-visible lookup digest of the code (never a credential).
    pub lookup_digest: [u8; 32],
}

impl std::fmt::Debug for CreatedInvite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedInvite")
            .field("display", &"<redacted: bearer secret>")
            .field("hint", &self.hint)
            .field("code_id", &hex(&self.code_id))
            .field("expires_ms", &self.expires_ms)
            .field("revision", &self.revision)
            .field("lookup_digest", &"<32-byte public digest>")
            .finish()
    }
}

/// A pairing transaction whose server commit is verified but whose local
/// apply has not run yet (the crash-recovery checkpoint of the redeemer
/// path; [`PairingOrchestrator::redeem`] is this plus the finish).
#[derive(Debug, Clone)]
pub struct CommittedPairing {
    /// The transaction.
    pub pairing_id: [u8; 16],
    /// The fully verified record (transcript + both proofs).
    pub verified: VerifiedPairing,
    /// Authoritative commit revision.
    pub commit_revision: u64,
    /// Fingerprint of the peer this device paired with.
    pub peer_fingerprint: Fingerprint,
}

/// Result of [`PairingOrchestrator::redeem_task`] (wave 28): the finished
/// pairing plus the initiator's endpoint blob as relayed by the registry.
#[derive(Debug, Clone)]
pub struct TaskRedeem {
    /// The fully verified, applied pairing.
    pub verified: VerifiedPairing,
    /// The initiator's endpoint blob carried by `PairCommit`; `None` when
    /// the code was claimed by a pre-wave-28 peer.
    pub owner_endpoint: Option<Vec<u8>>,
}

/// One redeem offer presented to the owner loop **before** the owner flow
/// starts (wave 29, review 2026-11-06 P1-05): what an installed
/// [`RedeemOfferGate`] sees of the relayed `InviteRedeemOffered`.
#[derive(Debug, Clone)]
pub struct RedeemOffer {
    /// The code being redeemed.
    pub code_id: [u8; 16],
    /// Registry revision of the code at offer time.
    pub code_revision: u64,
    /// The server-assigned pairing transaction.
    pub pairing_id: [u8; 16],
    /// The redeemer's authenticated device key.
    pub redeemer_device_id: [u8; 32],
    /// The redeemer's wave-28 endpoint blob as relayed by the offer's
    /// extension section (`None` when the redeemer predates the
    /// extension). Peeked, not consumed: a flow that proceeds still
    /// takes it exactly once.
    pub redeemer_endpoint: Option<Vec<u8>>,
}

/// Pre-commit veto over one redeem offer (see [`RedeemOffer`]). `Err`
/// aborts the pairing server-side before the owner flow (and therefore
/// before the server's commit) starts.
pub type RedeemOfferGate = Arc<dyn Fn(&RedeemOffer) -> Result<(), PairFlowError> + Send + Sync>;

/// In-memory invite registry: `secret_ref → code` plus `code_id` metadata.
#[derive(Debug, Default)]
struct Secrets {
    slots: HashMap<[u8; 16], InviteCode>,
    codes: HashMap<[u8; 16], CodeSlotMeta>,
    /// Pairing ids with a live owner flow (offer dedup across reconnect
    /// catch-up re-pushes).
    active_flows: HashMap<[u8; 16], ()>,
    /// `request_id to code` map for in-flight invite registrations, so a
    /// network retry with the same idempotency key re-sends the *same*
    /// digest (a fresh code under a reused id would be REQUEST_CONFLICT).
    requests: HashMap<[u8; 16], InviteCode>,
}

#[derive(Debug, Clone, Copy)]
struct CodeSlotMeta {
    secret_ref: [u8; 16],
    revision: u64,
    #[allow(dead_code)]
    expires_ms: u64,
}

/// The pairing orchestrator (see the module docs).
#[derive(Clone)]
pub struct PairingOrchestrator {
    client: RegistryClient,
    key: DeviceKeyPair,
    journal: Arc<PairingJournal>,
    /// The journal directory itself: alongside the `.akpair` entries it
    /// holds the `<pairing hex>.akdgm` authenticated-transcript pins the
    /// no-code recovery path relies on (see [`PairingOrchestrator::recover`]).
    journal_dir: PathBuf,
    book_path: PathBuf,
    peer_store: PeerBookStore,
    config: OrchestratorConfig,
    secrets: Arc<StdMutex<Secrets>>,
    /// Live backfill for codes this process did not create (wave 27):
    /// consulted when an offer names a code missing from `secrets`. The
    /// production daemon points this at `<id-dir>/invites.store`, so a
    /// code another process created (or revoked — the line removed) is
    /// answered (or declined) exactly like the wave-21 per-offer store
    /// lookup did.
    persisted_source: Arc<StdMutex<Option<PersistedCodeSource>>>,
    /// Optional pre-commit veto over redeem offers (wave 29, P1-05):
    /// consulted by every owner loop BEFORE the owner flow starts; a
    /// refusal aborts the transaction instead of answering it, so the
    /// server never commits and neither side journals/books.
    offer_gate: Arc<StdMutex<Option<RedeemOfferGate>>>,
}

/// Live lookup of one persisted invite code (see
/// `PairingOrchestrator::persisted_source`): `None` = not held (never
/// created here, expired, or revoked).
pub type PersistedCodeSource =
    Arc<dyn Fn(&[u8; 16]) -> Option<(InviteCode, u64, u64)> + Send + Sync>;

impl PairingOrchestrator {
    /// Open (creating) the journal directory and trust book under `id_dir`.
    pub fn open(
        client: RegistryClient,
        key: DeviceKeyPair,
        id_dir: &Path,
        config: OrchestratorConfig,
    ) -> std::io::Result<Self> {
        let journal_dir = id_dir.join(JOURNAL_SUBDIR);
        Ok(PairingOrchestrator {
            journal: Arc::new(PairingJournal::open(&journal_dir)?),
            journal_dir,
            book_path: id_dir.join(BOOK_NAME),
            client,
            key,
            peer_store: PeerBookStore::new(),
            config,
            secrets: Arc::new(StdMutex::new(Secrets::default())),
            persisted_source: Arc::new(StdMutex::new(None)),
            offer_gate: Arc::new(StdMutex::new(None)),
        })
    }

    /// Install the live persisted-code source (see `Self::persisted_source`).
    pub fn set_persisted_code_source(&self, source: PersistedCodeSource) {
        *self.persisted_source.lock().expect("orchestrator lock") = Some(source);
    }

    /// Install the pre-commit redeem-offer gate (wave 29, review
    /// 2026-11-06 P1-05). Every owner loop of this orchestrator consults
    /// the gate before answering an `InviteRedeemOffered`; `Err` aborts
    /// the pairing server-side (tombstoning the pending transaction)
    /// instead of driving it to commit. Last writer wins; pass-through
    /// when never installed.
    pub fn set_redeem_offer_gate(&self, gate: RedeemOfferGate) {
        *self.offer_gate.lock().expect("orchestrator lock") = Some(gate);
    }

    /// Open with the default tunables.
    pub fn with_defaults(
        client: RegistryClient,
        key: DeviceKeyPair,
        id_dir: &Path,
    ) -> std::io::Result<Self> {
        Self::open(client, key, id_dir, OrchestratorConfig::default())
    }

    /// The shared control client (the same instance the runtime transport
    /// wraps).
    pub fn client(&self) -> &RegistryClient {
        &self.client
    }

    /// Local trust-book path.
    pub fn book_path(&self) -> &Path {
        &self.book_path
    }

    // ---- invite management -------------------------------------------------

    /// Generate, register and remember one invite code. `CODE_CONFLICT`
    /// (a high-entropy registration collision) regenerates, at most
    /// [`OrchestratorConfig::claim_conflict_retries`] times. The plaintext
    /// code stays endpoint-side; only its lookup digest travels.
    pub async fn create_invite(&self, ttl_secs: u32) -> Result<CreatedInvite, PairFlowError> {
        self.create_invite_with_request_id(random_id16(), ttl_secs)
            .await
    }

    /// [`PairingOrchestrator::create_invite`] with a caller-chosen
    /// idempotency key: re-sending the same `request_id` (same digest,
    /// same ttl) replays the server's stored answer, so a network retry
    /// registers exactly one code.
    pub async fn create_invite_with_request_id(
        &self,
        request_id: [u8; 16],
        ttl_secs: u32,
    ) -> Result<CreatedInvite, PairFlowError> {
        self.create_invite_extended(request_id, ttl_secs, None, 0)
            .await
    }

    /// Wave-28 task-code claim: the v1 minimal pairing model. Identical to
    /// [`PairingOrchestrator::create_invite_with_request_id`] except the
    /// claim frame carries the initiator's endpoint blob (stored on the
    /// code row, relayed to the joiner inside `PairCommit`) and
    /// `max_redeems = 1` — one code pairs exactly once; a second redeem
    /// is refused with the dedicated `REDEEM_EXHAUSTED` business error.
    pub async fn create_task_invite(
        &self,
        request_id: [u8; 16],
        ttl_secs: u32,
        endpoint: ak_signal::protocol_v2::EndpointBlob,
    ) -> Result<CreatedInvite, PairFlowError> {
        self.create_invite_extended(request_id, ttl_secs, Some(endpoint), 1)
            .await
    }

    /// Shared claim path (plain or task-shaped).
    async fn create_invite_extended(
        &self,
        request_id: [u8; 16],
        ttl_secs: u32,
        endpoint: Option<ak_signal::protocol_v2::EndpointBlob>,
        max_redeems: u32,
    ) -> Result<CreatedInvite, PairFlowError> {
        let mut request_id = request_id;
        let mut budget = self.config.claim_conflict_retries.max(1);
        loop {
            // Retry of a known request id re-sends the same code (same
            // digest); a fresh id generates a fresh code.
            let code = {
                let secrets = self.secrets.lock().expect("orchestrator lock");
                secrets
                    .requests
                    .get(&request_id)
                    .cloned()
                    .unwrap_or_else(InviteCode::generate)
            };
            match self
                .client
                .invite_claim_task(
                    request_id,
                    code.lookup_digest(),
                    ttl_secs,
                    endpoint.clone(),
                    max_redeems,
                )
                .await
            {
                Ok(claimed) => {
                    let secret_ref = random_id16();
                    let mut secrets = self.secrets.lock().expect("orchestrator lock");
                    secrets.slots.insert(secret_ref, code.clone());
                    secrets.requests.insert(request_id, code.clone());
                    secrets.codes.insert(
                        claimed.code_id,
                        CodeSlotMeta {
                            secret_ref,
                            revision: claimed.revision,
                            expires_ms: claimed.expires_ms,
                        },
                    );
                    return Ok(CreatedInvite {
                        display: code.to_display(),
                        hint: code.code_hint(),
                        code_id: claimed.code_id,
                        expires_ms: claimed.expires_ms,
                        revision: claimed.revision,
                        lookup_digest: code.lookup_digest(),
                    });
                }
                Err(ClientError::Pair { code, .. }) if code == CODE_CONFLICT => {
                    budget -= 1;
                    if budget == 0 {
                        return Err(PairFlowError::Rejected { code });
                    }
                    // A regenerated code is a new logical registration.
                    request_id = random_id16();
                }
                Err(other) => return Err(map_client(other)),
            }
        }
    }

    /// Revoke one of this device's codes (pass-through of
    /// `InviteRevoke`; pending redeems of the code are aborted
    /// server-side with the unified `PAIR_UNAVAILABLE`).
    pub async fn revoke(&self, code_id: [u8; 16]) -> Result<RevokedInvite, PairFlowError> {
        let revision = {
            let secrets = self.secrets.lock().expect("orchestrator lock");
            secrets
                .codes
                .get(&code_id)
                .map(|meta| meta.revision)
                .ok_or_else(|| PairFlowError::Local("unknown invite code".into()))?
        };
        self.client
            .invite_revoke(code_id, revision)
            .await
            .map_err(map_client)
    }

    // ---- redeemer path -------------------------------------------------------

    /// Redeem a typed invite code and drive the transcript all the way to
    /// `PairReady` (see the module docs for the sequence).
    pub async fn redeem(&self, code_str: &str) -> Result<VerifiedPairing, PairFlowError> {
        let mut plane = self.client.subscribe_plane();
        let started = Instant::now();
        let committed = self
            .redeem_to_commit_on(code_str, &mut plane, None, started)
            .await?;
        self.finish_as_joiner(committed, &mut plane, started).await
    }

    /// Wave-28 task-code redeem: the v1 minimal pairing model. Identical
    /// to [`PairingOrchestrator::redeem`] except the redeem frame carries
    /// the joiner's endpoint blob (forwarded to the code owner inside
    /// `InviteRedeemOffered`), and the returned [`TaskRedeem`] also hands
    /// back the initiator's blob the registry relayed inside `PairCommit`
    /// (`None` when the initiator predates the wave-28 extension).
    pub async fn redeem_task(
        &self,
        code_str: &str,
        endpoint: ak_signal::protocol_v2::EndpointBlob,
    ) -> Result<TaskRedeem, PairFlowError> {
        let mut plane = self.client.subscribe_plane();
        let started = Instant::now();
        let committed = self
            .redeem_to_commit_on(code_str, &mut plane, Some(endpoint), started)
            .await?;
        let owner_endpoint = self.client.take_endpoint_blob(committed.pairing_id);
        let verified = self
            .finish_as_joiner(committed, &mut plane, started)
            .await?;
        Ok(TaskRedeem {
            verified,
            owner_endpoint,
        })
    }

    /// [`PairingOrchestrator::redeem`] stopping right after the verified
    /// `PairCommit` — the checkpoint a crashed redeemer resumes from.
    pub async fn redeem_to_commit(
        &self,
        code_str: &str,
    ) -> Result<CommittedPairing, PairFlowError> {
        let mut plane = self.client.subscribe_plane();
        self.redeem_to_commit_on(code_str, &mut plane, None, Instant::now())
            .await
    }

    /// Finish a committed pairing: trust-book apply, journal marks, the
    /// `PairApplied` confirmation, and the wait for `PairReady`.
    pub async fn finish_committed(
        &self,
        committed: CommittedPairing,
        plane: &mut mpsc::Receiver<CtrlMessage>,
    ) -> Result<VerifiedPairing, PairFlowError> {
        let mut last_started = FinishStep::CommitPeer;
        self.finish_committed_from(committed, plane, &mut last_started)
            .await
    }

    /// The joiner's finish of a committed pairing — the half both joiner
    /// entry points ([`Self::redeem`] and [`Self::redeem_task`]) share —
    /// with its closing trace line. The registry has committed by now, so
    /// a stop here is a pairing that stands server-side but not on this
    /// device; the line says which step stopped, with which failure, and
    /// returns the failure unchanged. Written here and nowhere else: the
    /// half before the commit has its own line
    /// ([`Self::redeem_to_commit_on`]), and the owner's outer wrapper
    /// writes the owner's.
    async fn finish_as_joiner(
        &self,
        committed: CommittedPairing,
        plane: &mut mpsc::Receiver<CtrlMessage>,
        started: Instant,
    ) -> Result<VerifiedPairing, PairFlowError> {
        let pairing_id = committed.pairing_id;
        let mut last_started = FinishStep::CommitPeer;
        let result = self
            .finish_committed_from(committed, plane, &mut last_started)
            .await;
        if let Err(error) = &result {
            // Q-N3 (review 21): the joiner's flow can stop after the
            // registry committed; that stretch was outside every closing
            // line.
            pair_diag!(
                "redeemer: flow stopped with `{}` {} ms after it began, at {} ms on the clock; \
                 after the commit, last stage started: {}, pairing {}",
                error_category(error),
                started.elapsed().as_millis(),
                now_unix_ms(),
                last_started.name(),
                id_prefix(&pairing_id)
            );
        }
        result
    }

    /// [`Self::finish_committed`] step by step, naming in `last_started`
    /// the step that started last — so a stop is placed after the step
    /// that ran, not before the one that did not. `PairReady` received and
    /// the local `applied` mark are two steps: the first can stand without
    /// the second.
    async fn finish_committed_from(
        &self,
        committed: CommittedPairing,
        plane: &mut mpsc::Receiver<CtrlMessage>,
        last_started: &mut FinishStep,
    ) -> Result<VerifiedPairing, PairFlowError> {
        let transcript = committed.verified.record().transcript;
        let pairing_id = committed.pairing_id;
        let digest = transcript.transcript_digest();
        let me = self.client.device_id();
        let peer_pub = if transcript.owner_device_id == me {
            transcript.redeemer_device_id
        } else {
            transcript.owner_device_id
        };
        // Which side of the transcript this device is names its flow in the
        // trace; the owner finishes through here too.
        let flow = if transcript.owner_device_id == me {
            "owner"
        } else {
            "redeemer"
        };
        *last_started = FinishStep::CommitPeer;
        self.commit_peer(&pairing_id, peer_pub)?;
        *last_started = FinishStep::MarkCommitted;
        self.journal.mark_committed(&pairing_id)?;
        let signature = self.key.sign(&hello2::applied_message(
            &pairing_id,
            &digest,
            committed.commit_revision,
        ));
        *last_started = FinishStep::SendApplied;
        self.client
            .send_pair_applied(pairing_id, digest, committed.commit_revision, signature)
            .await
            .map_err(map_client)?;
        *last_started = FinishStep::WaitReady;
        wait_ready(
            &self.client,
            plane,
            pairing_id,
            digest,
            self.config.ready_wait_ms,
            flow,
        )
        .await?;
        pair_diag!(
            "applied sent and ready received, {} ms from the deadline; flow: {}, pairing {}",
            from_deadline_ms(transcript.deadline_ms),
            flow,
            id_prefix(&pairing_id)
        );
        *last_started = FinishStep::MarkApplied;
        self.journal.mark_applied(&pairing_id)?;
        Ok(committed.verified)
    }

    async fn redeem_to_commit_on(
        &self,
        code_str: &str,
        plane: &mut mpsc::Receiver<CtrlMessage>,
        endpoint: Option<ak_signal::protocol_v2::EndpointBlob>,
        started: Instant,
    ) -> Result<CommittedPairing, PairFlowError> {
        let result = self
            .redeem_to_commit_on_inner(code_str, plane, endpoint)
            .await;
        if let Err(error) = &result {
            // Q-N3: the last step is in the lines above; this is where the
            // joiner's flow stopped and how long it took.
            pair_diag!(
                "redeemer: flow stopped with `{}` {} ms after it began, at {} ms on the clock",
                error_category(error),
                started.elapsed().as_millis(),
                now_unix_ms()
            );
        }
        result
    }

    async fn redeem_to_commit_on_inner(
        &self,
        code_str: &str,
        plane: &mut mpsc::Receiver<CtrlMessage>,
        endpoint: Option<ak_signal::protocol_v2::EndpointBlob>,
    ) -> Result<CommittedPairing, PairFlowError> {
        let code = InviteCode::parse(code_str)?;
        let auth_key = code.auth_key();
        let registry = self.client.registry_id();
        let me = self.client.device_id();
        let request_id = random_id16();
        let nonce = random_nonce32();
        let window = self.config.redeem_window_ms.clamp(1, 119_000);
        let deadline = now_unix_ms().saturating_add(window);
        let request_mac = redeem_request_mac(
            &auth_key,
            &registry,
            &code.lookup_digest(),
            &me,
            &request_id,
            &nonce,
            deadline,
        );
        let signature = self.key.sign(&hello2::redeem_message(
            &registry,
            &code.lookup_digest(),
            &me,
            &request_id,
            &nonce,
            deadline,
            &request_mac,
        ));
        let request = ak_signal::protocol_v2::RedeemRequest {
            request_id,
            lookup_digest: code.lookup_digest(),
            redeemer_nonce: nonce,
            deadline_ms: deadline,
            request_mac,
            signature,
        };
        let ack = match endpoint {
            Some(blob) => self.client.invite_redeem_with_endpoint(request, blob).await,
            None => self.client.invite_redeem(request).await,
        }
        .map_err(map_client)?;
        let pairing_id = ack.pairing_id;
        pair_diag!(
            "redeemer: redeem acknowledged, {} ms from the deadline",
            from_deadline_ms(deadline)
        );

        // ← PairChallenge (relayed owner transcript + owner proof).
        let (transcript, owner_proof) = match wait_frame(
            &self.client,
            plane,
            pairing_id,
            deadline.saturating_add(5_000),
            Waiting {
                flow: "redeemer",
                frame: "PairChallenge",
            },
            |frame| pairing_challenge_of(frame, pairing_id),
        )
        .await?
        {
            Some(pair) => pair,
            None => return Err(PairFlowError::Timeout),
        };
        let transcript = PairTranscript::from_bytes(&transcript);
        let bindings_ok = transcript.pairing_id == pairing_id
            && transcript.lookup_digest == code.lookup_digest()
            && transcript.redeemer_device_id == me
            && transcript.request_id == request_id
            && transcript.redeemer_nonce == nonce
            && transcript.deadline_ms == deadline;
        if !bindings_ok {
            return Err(PairFlowError::Local(
                "challenge transcript does not bind this redeem".into(),
            ));
        }
        verify_endpoint_proof(
            &auth_key,
            OWNER_PROOF_DOMAIN,
            &transcript,
            &owner_proof,
            &transcript.owner_device_id,
        )?;
        pair_diag!(
            "redeemer: challenge received and the owner proof verified, {} ms from the deadline",
            from_deadline_ms(deadline)
        );

        // Pin the authenticated transcript locally BEFORE the journal
        // entry (P1-1): the no-code recovery path later compares the
        // re-sent PairCommit against this digest instead of trusting the
        // transcript's self-declared keys. Written first so a journal
        // entry always implies a pin (an orphan pin without an entry is
        // harmless).
        self.remember_authenticated_transcript(&transcript)?;

        // Journal before exchanging proofs (the crash window that matters).
        let secret_ref = self.remember_code(&code);
        self.journal.record_pending(&PendingPairing {
            pairing_id,
            code_id: transcript.code_id,
            lookup_digest: code.lookup_digest(),
            secret_ref,
            peer_public_key: Some(transcript.owner_device_id),
        })?;
        pair_diag!(
            "redeemer: pin and journal written, {} ms from the deadline",
            from_deadline_ms(deadline)
        );

        // → PairProof + PairPrepared.
        let digest = transcript.transcript_digest();
        let record = local_half_record(&transcript, &auth_key, &self.key, Role::Redeemer);
        self.client
            .send_pair_proof(pairing_id, digest, wire_proof(record.redeemer_proof))
            .await
            .map_err(map_client)?;
        let prepared = self
            .key
            .sign(&hello2::prepared_message(&pairing_id, &digest));
        self.client
            .send_pair_prepared(pairing_id, digest, prepared)
            .await
            .map_err(map_client)?;
        pair_diag!(
            "redeemer: proof and prepared sent, {} ms from the deadline",
            from_deadline_ms(deadline)
        );

        // ← PairCommit (authoritative).
        let commit = match wait_frame(
            &self.client,
            plane,
            pairing_id,
            deadline.saturating_add(COMMIT_DELIVERY_GRACE_MS),
            Waiting {
                flow: "redeemer",
                frame: "PairCommit",
            },
            |frame| pair_commit_of(frame, pairing_id),
        )
        .await?
        {
            Some(commit) => commit,
            None => return Err(PairFlowError::Timeout),
        };
        let verified = verify_live_commit(
            &commit,
            &transcript,
            &auth_key,
            self.client.frame_taken_at_unix_ms(),
        )?;
        pair_diag!(
            "redeemer: commit received and verified, {} ms from the deadline",
            from_deadline_ms(deadline)
        );
        Ok(CommittedPairing {
            pairing_id,
            peer_fingerprint: Fingerprint::from_public_key(&transcript.owner_device_id),
            commit_revision: commit.commit_revision,
            verified,
        })
    }

    // ---- owner path ----------------------------------------------------------

    /// Adopt a code this device created in an EARLIER process run (wave 27,
    /// integrator: production `daemon --pairing`). `Secrets` is in-memory
    /// and opens empty — codes registered through the persisted
    /// `<id-dir>/invites.store` (the CLI `invite create` flow) must be fed
    /// back before [`Self::spawn_owner_loop`] can answer their redeems.
    /// The revision/expiry metadata comes from the persisted record; the
    /// registry remains authoritative on both at answer time.
    pub fn adopt_persisted_code(
        &self,
        code: &InviteCode,
        code_id: [u8; 16],
        revision: u64,
        expires_ms: u64,
    ) {
        let mut secrets = self.secrets.lock().expect("orchestrator lock");
        // Same dedup as remember_code: a re-adoption of the identical
        // secret (store reload, daemon restart) is idempotent.
        let secret_ref = 'found: {
            for (secret_ref, held) in &secrets.slots {
                if held.lookup_digest() == code.lookup_digest() {
                    break 'found *secret_ref;
                }
            }
            let secret_ref = random_id16();
            secrets.slots.insert(secret_ref, code.clone());
            secret_ref
        };
        secrets.codes.insert(
            code_id,
            CodeSlotMeta {
                secret_ref,
                revision,
                expires_ms,
            },
        );
    }

    /// Spawn the resident owner loop: consumes the pairing plane, answers
    /// every `InviteRedeemOffered` for a code this device holds. Duplicate
    /// offers (Hello catch-up re-pushes) are deduplicated per pairing id.
    pub fn spawn_owner_loop(&self) -> JoinHandle<()> {
        self.spawn_owner_loop_notifying(None)
    }

    /// [`Self::spawn_owner_loop`] with an optional success callback: the
    /// redeemer's fingerprint is reported once its pairing committed (the
    /// production daemon prints its `paired with redeemer …` line here;
    /// the cross-device script's pairing barrier waits on that text).
    pub fn spawn_owner_loop_notifying(
        &self,
        on_paired: Option<std::sync::Arc<dyn Fn(Fingerprint) + Send + Sync>>,
    ) -> JoinHandle<()> {
        self.spawn_owner_loop_inner(on_paired.map(|sink| {
            std::sync::Arc::new(move |verified: VerifiedPairing| {
                sink(verified.redeemer_fingerprint())
            }) as std::sync::Arc<dyn Fn(VerifiedPairing) + Send + Sync>
        }))
    }

    /// Wave-28 owner loop with the full verified record reported per
    /// committed pairing (the task layer reads the redeemer's endpoint
    /// blob off the relayed offer through
    /// [`RegistryClient::take_endpoint_blob`]).
    pub fn spawn_owner_loop_verified(
        &self,
        on_paired: Option<std::sync::Arc<dyn Fn(VerifiedPairing) + Send + Sync>>,
    ) -> JoinHandle<()> {
        self.spawn_owner_loop_inner(on_paired)
    }

    /// Shared owner-loop body (fingerprint- and record-shaped wrappers
    /// above).
    fn spawn_owner_loop_inner(
        &self,
        on_paired: Option<std::sync::Arc<dyn Fn(VerifiedPairing) + Send + Sync>>,
    ) -> JoinHandle<()> {
        let orchestrator = self.clone();
        // Subscribe synchronously: an offer pushed between `spawn` and the
        // task's first poll would otherwise be dropped with no subscriber.
        let mut plane = orchestrator.client.subscribe_plane();
        tokio::spawn(async move {
            loop {
                let Some(frame) = plane.recv().await else {
                    break;
                };
                if let CtrlMessage::InviteRedeemOffered {
                    code_id,
                    code_revision,
                    pairing_id,
                    redeemer_device_id,
                    redeem,
                } = frame
                {
                    let fresh = orchestrator
                        .secrets
                        .lock()
                        .expect("orchestrator lock")
                        .active_flows
                        .insert(pairing_id, ())
                        .is_none();
                    if !fresh {
                        continue;
                    }
                    // Pre-commit gate (wave 29, P1-05): a refusing gate
                    // vetoes the offer BEFORE the owner flow starts. The
                    // owner never sends its `PairChallenge`/`PairPrepared`,
                    // so the server (which commits exactly when both
                    // `PairPrepared` frames have arrived) never commits;
                    // the aborted attempt does not consume the code's
                    // redeem budget, and neither side has journaled or
                    // booked anything at this point.
                    let gate = orchestrator
                        .offer_gate
                        .lock()
                        .expect("orchestrator lock")
                        .clone();
                    if let Some(gate) = gate {
                        let offer = RedeemOffer {
                            code_id,
                            code_revision,
                            pairing_id,
                            redeemer_device_id,
                            redeemer_endpoint: orchestrator.client.peek_endpoint_blob(pairing_id),
                        };
                        if let Err(err) = gate(&offer) {
                            log::warn!(
                                "redeem offer refused before the owner flow ({}): {err}",
                                hex(&pairing_id)
                            );
                            let client = orchestrator.client.clone();
                            tokio::spawn(async move {
                                // Tombstone the pending transaction so the
                                // server can never commit it; the redeemer
                                // observes the unified `PAIR_UNAVAILABLE`
                                // before any of its local writes.
                                let _ = client.pair_abort(pairing_id, AbortReason::Protocol).await;
                            });
                            continue;
                        }
                    }
                    let orchestrator = orchestrator.clone();
                    let on_paired = on_paired.clone();
                    tokio::spawn(async move {
                        match orchestrator
                            .owner_answer_offer(
                                code_id,
                                code_revision,
                                pairing_id,
                                redeemer_device_id,
                                redeem,
                            )
                            .await
                        {
                            Ok(verified) => {
                                if let Some(on_paired) = on_paired.as_ref() {
                                    // The redeemer is this daemon's peer
                                    // (the owner loop runs on the owner).
                                    on_paired(verified);
                                }
                            }
                            Err(err) => {
                                log::warn!(
                                    "owner pairing flow failed ({}): {err}",
                                    hex(&pairing_id)
                                );
                                let _ = orchestrator
                                    .client
                                    .pair_abort(pairing_id, AbortReason::Protocol)
                                    .await;
                            }
                        }
                    });
                }
            }
        })
    }

    /// Owner side of one transaction (see the module docs). Public for
    /// embedding; [`PairingOrchestrator::spawn_owner_loop`] drives it.
    pub async fn owner_answer_offer(
        &self,
        code_id: [u8; 16],
        code_revision: u64,
        pairing_id: [u8; 16],
        redeemer_device_id: [u8; 32],
        redeem: ak_signal::protocol_v2::RedeemRequest,
    ) -> Result<VerifiedPairing, PairFlowError> {
        let started = std::time::Instant::now();
        let result = self
            .owner_answer_offer_inner(
                code_id,
                code_revision,
                pairing_id,
                redeemer_device_id,
                redeem,
            )
            .await;
        if let Err(error) = &result {
            // Q-N3: where the owner's flow stopped and how long it took.
            pair_diag!(
                "owner: flow stopped with `{}` {} ms after the offer, at {} ms on the clock",
                error_category(error),
                started.elapsed().as_millis(),
                now_unix_ms()
            );
        }
        result
    }

    async fn owner_answer_offer_inner(
        &self,
        code_id: [u8; 16],
        code_revision: u64,
        pairing_id: [u8; 16],
        redeemer_device_id: [u8; 32],
        redeem: ak_signal::protocol_v2::RedeemRequest,
    ) -> Result<VerifiedPairing, PairFlowError> {
        let mut plane = self.client.subscribe_plane();
        let (auth_key, secret_ref, lookup_digest) = {
            let mut secrets = self.secrets.lock().expect("orchestrator lock");
            // Miss in the in-memory slots: codes created by ANOTHER process
            // (e.g. `ak-cli invite create` while this daemon runs) land in
            // the persisted store only — consult the backfill source before
            // declaring the code unknown (wave 27). The source reads the
            // store LIVE, so a revoked code (line removed) resolves to
            // None here and is declined exactly like the old per-offer
            // store lookup did.
            if !secrets.codes.contains_key(&code_id) {
                let persisted = self
                    .persisted_source
                    .lock()
                    .expect("orchestrator lock")
                    .clone();
                if let Some((code, revision, expires_ms)) =
                    persisted.as_ref().and_then(|source| source(&code_id))
                {
                    let secret_ref = random_id16();
                    secrets.slots.insert(secret_ref, code.clone());
                    secrets.codes.insert(
                        code_id,
                        CodeSlotMeta {
                            secret_ref,
                            revision,
                            expires_ms,
                        },
                    );
                }
            }
            let meta = secrets
                .codes
                .get(&code_id)
                .ok_or_else(|| PairFlowError::Local("offer for an unknown code".into()))?;
            let code = secrets
                .slots
                .get(&meta.secret_ref)
                .ok_or_else(|| PairFlowError::Local("invite secret no longer resident".into()))?;
            (code.auth_key(), meta.secret_ref, code.lookup_digest())
        };
        let registry = self.client.registry_id();
        let me = self.client.device_id();

        // The redeemer must prove knowledge of the invite *secret*, not just
        // its digest: recompute the request MAC under the real auth key.
        let expected_mac = redeem_request_mac(
            &auth_key,
            &registry,
            &lookup_digest,
            &redeemer_device_id,
            &redeem.request_id,
            &redeem.redeemer_nonce,
            redeem.deadline_ms,
        );
        if !constant_time_eq_32(&expected_mac, &redeem.request_mac) {
            return Err(PairFlowError::Verify(InviteCodeMismatch));
        }
        let signed = hello2::redeem_message(
            &registry,
            &lookup_digest,
            &redeemer_device_id,
            &redeem.request_id,
            &redeem.redeemer_nonce,
            redeem.deadline_ms,
            &redeem.request_mac,
        );
        if !DeviceKeyPair::verify(&redeemer_device_id, &signed, &redeem.signature) {
            return Err(PairFlowError::Verify(PublicKeyMismatch));
        }

        // Journal the transaction, then pin the counterparty.
        self.journal.record_pending(&PendingPairing {
            pairing_id,
            code_id,
            lookup_digest,
            secret_ref,
            peer_public_key: None,
        })?;
        self.journal
            .set_peer_public_key(&pairing_id, &redeemer_device_id)?;

        // Build the transcript and the owner proof.
        let owner_nonce = random_nonce32();
        let transcript = PairTranscript {
            registry_id: registry,
            code_id,
            code_revision,
            pairing_id,
            request_id: redeem.request_id,
            lookup_digest,
            owner_device_id: me,
            redeemer_device_id,
            owner_nonce,
            redeemer_nonce: redeem.redeemer_nonce,
            deadline_ms: redeem.deadline_ms,
        };
        let bytes = transcript.to_bytes();
        let digest = transcript.transcript_digest();
        let record = local_half_record(&transcript, &auth_key, &self.key, Role::Owner);
        // The owner constructed this transcript itself (own nonce, MACed
        // under the invite auth key); pin it before the first frame so a
        // crash anywhere later in the flow still leaves the local
        // binding recovery can verify against (P1-1).
        self.remember_authenticated_transcript(&transcript)?;
        self.client
            .send_pair_challenge(bytes, wire_proof(record.owner_proof))
            .await
            .map_err(map_client)?;
        pair_diag!(
            "owner: challenge sent, {} ms from the deadline",
            from_deadline_ms(redeem.deadline_ms)
        );

        // ← PairProof (redeemer's half).
        let redeemer_proof = match wait_frame(
            &self.client,
            &mut plane,
            pairing_id,
            redeem.deadline_ms.saturating_add(5_000),
            Waiting {
                flow: "owner",
                frame: "PairProof",
            },
            |frame| pair_proof_of(frame, pairing_id, digest),
        )
        .await?
        {
            Some(proof) => proof,
            None => return Err(PairFlowError::Timeout),
        };
        verify_endpoint_proof(
            &auth_key,
            REDEEMER_PROOF_DOMAIN,
            &transcript,
            &redeemer_proof,
            &transcript.redeemer_device_id,
        )?;
        pair_diag!(
            "owner: redeemer proof received and verified, {} ms from the deadline",
            from_deadline_ms(redeem.deadline_ms)
        );

        // → PairPrepared, then ← PairCommit.
        let prepared = self
            .key
            .sign(&hello2::prepared_message(&pairing_id, &digest));
        self.client
            .send_pair_prepared(pairing_id, digest, prepared)
            .await
            .map_err(map_client)?;
        pair_diag!(
            "owner: prepared sent, {} ms from the deadline",
            from_deadline_ms(redeem.deadline_ms)
        );
        let commit = match wait_frame(
            &self.client,
            &mut plane,
            pairing_id,
            redeem.deadline_ms.saturating_add(COMMIT_DELIVERY_GRACE_MS),
            Waiting {
                flow: "owner",
                frame: "PairCommit",
            },
            |frame| pair_commit_of(frame, pairing_id),
        )
        .await?
        {
            Some(commit) => commit,
            None => return Err(PairFlowError::Timeout),
        };
        let verified = verify_live_commit(
            &commit,
            &transcript,
            &auth_key,
            self.client.frame_taken_at_unix_ms(),
        )?;
        pair_diag!(
            "owner: commit received and verified, {} ms from the deadline",
            from_deadline_ms(redeem.deadline_ms)
        );
        let committed = CommittedPairing {
            pairing_id,
            peer_fingerprint: Fingerprint::from_public_key(&redeemer_device_id),
            commit_revision: commit.commit_revision,
            verified,
        };
        self.finish_committed(committed, &mut plane).await
    }

    // ---- recovery --------------------------------------------------------------

    /// Startup recovery over every journal entry that is not `applied`.
    ///
    /// `code_hints` are invite codes the operator can still supply (the
    /// running app kept the typed string). A hint matching a record's
    /// `lookup_digest` restores full MAC verification. Without a hint the
    /// recovery trusts only local evidence: the re-sent `PairCommit` must
    /// reproduce the record's pinned authenticated-transcript digest (see
    /// `PairingOrchestrator::remember_authenticated_transcript`)
    /// byte-for-byte. An entry with neither a code hint nor a pin is
    /// refused with an explicit error — the server's self-reported
    /// `Committed`/`Ready` state is never accepted as the only evidence
    /// (review 2026-11-05, P1-1). Returns the pairing ids brought to
    /// `applied`.
    pub async fn recover(&self, code_hints: &[InviteCode]) -> Result<Vec<[u8; 16]>, PairFlowError> {
        let mut applied = Vec::new();
        let mut hint_by_lookup: HashMap<[u8; 32], InviteCode> = HashMap::new();
        for code in code_hints {
            hint_by_lookup.insert(code.lookup_digest(), code.clone());
        }
        let records = self.journal.load_all()?;
        for record in records {
            if record.phase == PairingPhase::Applied {
                continue;
            }
            if !self.client.is_connected() {
                self.client.reconnect().await.map_err(map_client)?;
            }
            let pairing_id = record.pairing_id;
            // Subscribe before asking: the server re-sends the fixed
            // PairCommit/PairReady immediately after its reply.
            let mut plane = self.client.subscribe_plane();
            let status = match self.client.pair_status_query(pairing_id).await {
                Ok(status) => status,
                Err(ClientError::Pair { code, .. }) => {
                    log::info!(
                        "recovery: pairing {} is gone server-side (0x{code:04x})",
                        hex(&pairing_id)
                    );
                    continue;
                }
                Err(other) => return Err(map_client(other)),
            };
            match status.state {
                PairingState::Ready => {
                    // Even a Ready status needs local corroboration (a
                    // code hint, or this device's own transcript pin);
                    // otherwise the server's word alone would mark a
                    // pairing applied (P1-1).
                    if !hint_by_lookup.contains_key(&record.lookup_digest)
                        && self.authenticated_digest(&pairing_id)?.is_none()
                    {
                        return Err(recover_unverifiable());
                    }
                    if wait_ready(
                        &self.client,
                        &mut plane,
                        pairing_id,
                        [0u8; 32],
                        10_000,
                        "recovery",
                    )
                    .await
                    .is_ok()
                    {
                        self.journal.mark_applied(&pairing_id)?;
                        applied.push(pairing_id);
                    }
                }
                PairingState::Committed | PairingState::Applied | PairingState::Prepared => {
                    // The server re-sends the fixed PairCommit right after
                    // the status reply.
                    let commit = match wait_frame(
                        &self.client,
                        &mut plane,
                        pairing_id,
                        now_unix_ms().saturating_add(10_000),
                        Waiting {
                            flow: "recovery",
                            frame: "PairCommit",
                        },
                        |frame| pair_commit_of(frame, pairing_id),
                    )
                    .await?
                    {
                        Some(commit) => commit,
                        None => continue,
                    };
                    let transcript = PairTranscript::from_bytes(&commit.transcript);
                    let digest = transcript.transcript_digest();
                    match hint_by_lookup.get(&record.lookup_digest) {
                        Some(code) => {
                            // Full MAC verification; the expiry bound is the
                            // transcript's own deadline. The registry accepted
                            // the `PairPrepared` on an acceptance sample at or
                            // before that deadline and sent the frame only
                            // after its commit succeeded; a wire frame carries
                            // no commit time of its own.
                            let now = now_unix_ms().min(transcript.deadline_ms);
                            let _ = verify_commit_full(&commit, &code.auth_key(), now)?;
                        }
                        None => {
                            // No secret: the commit must reproduce the
                            // locally pinned digest of the transcript this
                            // device authenticated pre-crash. Without a pin
                            // there is nothing local to check against —
                            // refuse rather than fall back to trusting the
                            // transcript's self-declared keys (P1-1).
                            let pinned = self.authenticated_digest(&pairing_id)?;
                            match pinned {
                                Some(expected) => verify_commit_pinned(&commit, &expected)?,
                                None => return Err(recover_unverifiable()),
                            }
                        }
                    }
                    self.commit_peer(&pairing_id, peer_of(&transcript, self.client.device_id()))?;
                    self.journal.mark_committed(&pairing_id)?;
                    let signature = self.key.sign(&hello2::applied_message(
                        &pairing_id,
                        &digest,
                        commit.commit_revision,
                    ));
                    self.client
                        .send_pair_applied(pairing_id, digest, commit.commit_revision, signature)
                        .await
                        .map_err(map_client)?;
                    if wait_ready(
                        &self.client,
                        &mut plane,
                        pairing_id,
                        digest,
                        self.config.ready_wait_ms,
                        "recovery",
                    )
                    .await
                    .is_ok()
                    {
                        self.journal.mark_applied(&pairing_id)?;
                        applied.push(pairing_id);
                    }
                }
                PairingState::Pending | PairingState::Aborted => {
                    log::info!(
                        "recovery: pairing {} is {:?}; leaving it to the flows or the sweeper",
                        hex(&pairing_id),
                        status.state
                    );
                }
            }
        }
        Ok(applied)
    }

    // ---- internals ---------------------------------------------------------

    /// Remember (or reuse) the in-memory slot of a code this device typed.
    fn remember_code(&self, code: &InviteCode) -> [u8; 16] {
        let mut secrets = self.secrets.lock().expect("orchestrator lock");
        for (secret_ref, held) in &secrets.slots {
            if held.lookup_digest() == code.lookup_digest() {
                return *secret_ref;
            }
        }
        let secret_ref = random_id16();
        secrets.slots.insert(secret_ref, code.clone());
        secret_ref
    }

    /// Persist the digest of a transcript this device has authenticated
    /// (redeemer: owner proof MAC verified; owner: constructed locally)
    /// as the `<pairing hex>.akdgm` sidecar in the journal directory —
    /// the pin the no-code recovery path verifies the re-sent
    /// `PairCommit` against (P1-1). Failing closed: if the pin cannot be
    /// written, the flow errors instead of silently losing the pin.
    fn remember_authenticated_transcript(
        &self,
        transcript: &PairTranscript,
    ) -> Result<(), PairFlowError> {
        write_digest_sidecar(
            &self.journal_dir,
            &transcript.pairing_id,
            &transcript.transcript_digest(),
        )?;
        Ok(())
    }

    /// The pinned digest of the transcript this device authenticated for
    /// `pairing_id`, if any. `Ok(None)` = no pin (never authenticated, or
    /// a pre-pin journal). A present-but-corrupt pin is `Err` — fail
    /// closed, never silently treat it as absent.
    fn authenticated_digest(&self, pairing_id: &[u8; 16]) -> io::Result<Option<[u8; 32]>> {
        read_digest_sidecar(&self.journal_dir, pairing_id)
    }

    /// Transactional trust-book write (idempotent per pairing id).
    fn commit_peer(&self, pairing_id: &[u8; 16], peer_pub: [u8; 32]) -> Result<(), PairFlowError> {
        self.peer_store
            .commit_paired_peer(
                &self.book_path,
                PeerRecord {
                    fingerprint: Fingerprint::from_public_key(&peer_pub),
                    name: String::new(),
                    paired_unix_ms: now_unix_ms(),
                },
                pairing_id,
            )
            .map_err(|err| PairFlowError::Local(format!("trust book commit failed: {err}")))?;
        Ok(())
    }
}

// ---- flow-role helpers -----------------------------------------------------

/// Which half of the transcript this device signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Owner,
    Redeemer,
}

/// Build the record with only the local half meaningful:
/// `ak-identity`'s only public proof constructor signs **both** halves and
/// needs both private keys, but each endpoint holds exactly one. The
/// unused half is signed with a throwaway key and discarded — the peer's
/// real proof arrives over the wire and the commit is verified as a whole.
fn local_half_record(
    transcript: &PairTranscript,
    auth_key: &[u8; 32],
    key: &DeviceKeyPair,
    role: Role,
) -> PairingRecord {
    let throwaway = DeviceKeyPair::generate();
    match role {
        Role::Owner => PairingRecord::build(*transcript, auth_key, key, &throwaway),
        Role::Redeemer => PairingRecord::build(*transcript, auth_key, &throwaway, key),
    }
}

/// Verify one endpoint's 96-byte proof (MAC under the invite auth key plus
/// the device signature) against a transcript.
fn verify_endpoint_proof(
    auth_key: &[u8; 32],
    domain: &str,
    transcript: &PairTranscript,
    proof: &WireProof,
    claimed_device: &[u8; 32],
) -> Result<(), PairFlowError> {
    let bytes = transcript.to_bytes();
    let expected_mac = proof_mac(auth_key, domain, &bytes);
    if !constant_time_eq_32(&expected_mac, &proof.mac) {
        return Err(PairFlowError::Verify(InviteCodeMismatch));
    }
    let signed = hello2::proof_signed_message(domain, &bytes, &proof.mac);
    if !DeviceKeyPair::verify(claimed_device, &signed, &proof.signature) {
        return Err(PairFlowError::Verify(PublicKeyMismatch));
    }
    Ok(())
}

/// The relevant fields of one received `PairCommit`.
struct CommitView {
    transcript: [u8; 256],
    owner_proof: WireProof,
    redeemer_proof: WireProof,
    commit_revision: u64,
}

/// How long after the deadline a live flow still waits for, and still
/// takes, the `PairCommit` of its own round (Z-N7). The registry takes a
/// `PairPrepared` only while its own acceptance sample reads at or before
/// the deadline, commits, and sends the frame after that; the frame can
/// reach an endpoint later. The bound is one and the same for the wait and
/// for the delivery, so a frame taken off the wire past it is a timeout,
/// never a proof failure.
const COMMIT_DELIVERY_GRACE_MS: u64 = 10_000;

/// Milliseconds between now and the redeem deadline: negative is before it.
fn from_deadline_ms(deadline_ms: u64) -> i64 {
    now_unix_ms() as i64 - deadline_ms as i64
}

/// Verification of a `PairCommit` taken off the wire by a live flow
/// (`redeem_to_commit_on` and `owner_answer_offer`).
///
/// `taken_at_unix_ms` is when this endpoint took the frame, not when it
/// left the registry. Checked in this order:
///
/// 1. Past `COMMIT_DELIVERY_GRACE_MS` after THIS round's deadline (the
///    deadline of the transcript the flow holds) the flow reports
///    `Timeout`, whatever the frame carries. The wait has the same bound,
///    but a frame queued before the timer fired can still be taken after it.
/// 2. The frame must carry THIS round's transcript: the 256 canonical
///    bytes the flow authenticated before it sent its `PairPrepared`.
///    `pair_commit_of` only matches the pairing id, and the proof check
///    takes its digest and nonces from the frame itself, so a commit of
///    another round under the same pairing id passes the bare check — it
///    must not pass here.
/// 3. Inside the grace the proof check runs at `min(taken at, deadline)`.
///    What the registry guarantees of a frame of this round: it accepted
///    this round's `PairPrepared` on an acceptance sample at or before the
///    deadline, and it sent the frame only after the commit of that
///    acceptance succeeded. The nonces bind the frame to this attempt. A
///    wire frame carries no commit time, and the registry does not sample
///    again before it commits, so the endpoint cannot place the commit
///    itself before the deadline — this is as much as it can establish.
fn verify_live_commit(
    commit: &CommitView,
    round_transcript: &PairTranscript,
    auth_key: &[u8; 32],
    taken_at_unix_ms: u64,
) -> Result<VerifiedPairing, PairFlowError> {
    let deadline = round_transcript.deadline_ms;
    if taken_at_unix_ms > deadline.saturating_add(COMMIT_DELIVERY_GRACE_MS) {
        return Err(PairFlowError::Timeout);
    }
    if commit.transcript != round_transcript.to_bytes() {
        return Err(PairFlowError::Verify(
            ak_identity::pairing_txn::PairingVerifyError::TranscriptDigestMismatch,
        ));
    }
    verify_commit_full(commit, auth_key, taken_at_unix_ms.min(deadline))
}

/// Full verification of a `PairCommit` against the invite auth key.
fn verify_commit_full(
    commit: &CommitView,
    auth_key: &[u8; 32],
    now_unix_ms: u64,
) -> Result<VerifiedPairing, PairFlowError> {
    let transcript = PairTranscript::from_bytes(&commit.transcript);
    let record = PairingRecord::from_pieces(
        transcript,
        transcript.transcript_digest(),
        unwire_proof(&commit.owner_proof),
        unwire_proof(&commit.redeemer_proof),
    );
    let expectations = VerifyExpectations {
        auth_key,
        expected_owner_nonce: &transcript.owner_nonce,
        expected_redeemer_nonce: &transcript.redeemer_nonce,
        now_unix_ms,
    };
    Ok(record.verify_transcript(&expectations)?)
}

/// Recovery verification without the invite secret (P1-1): the received
/// `PairCommit` transcript must reproduce the locally pinned digest of a
/// transcript this device authenticated pre-crash — BLAKE3 digest
/// equality over the 256-byte canonical transcript is a byte-for-byte
/// binding, so a registry that swaps device keys (and re-signs under
/// its own keys, which the pre-fix degraded check happily accepted
/// because the keys were self-attested) cannot pass. Both Ed25519
/// signatures are still verified under the keys the now-pinned
/// transcript names — defense in depth, no longer self-attestation.
fn verify_commit_pinned(commit: &CommitView, pinned: &[u8; 32]) -> Result<(), PairFlowError> {
    let transcript = PairTranscript::from_bytes(&commit.transcript);
    if &transcript.transcript_digest() != pinned {
        return Err(PairFlowError::Verify(
            ak_identity::pairing_txn::PairingVerifyError::TranscriptDigestMismatch,
        ));
    }
    let bytes = transcript.to_bytes();
    for (domain, proof, device) in [
        (
            OWNER_PROOF_DOMAIN,
            &commit.owner_proof,
            transcript.owner_device_id,
        ),
        (
            REDEEMER_PROOF_DOMAIN,
            &commit.redeemer_proof,
            transcript.redeemer_device_id,
        ),
    ] {
        let signed = hello2::proof_signed_message(domain, &bytes, &proof.mac);
        if !DeviceKeyPair::verify(&device, &signed, &proof.signature) {
            return Err(PairFlowError::Verify(PublicKeyMismatch));
        }
    }
    Ok(())
}

/// The explicit failure for a journal entry that can no longer be
/// verified locally: no invite code to re-run the MACs, and no pinned
/// authenticated transcript. The server's self-reported state is not
/// evidence (P1-1).
fn recover_unverifiable() -> PairFlowError {
    PairFlowError::Local("cannot verify without invite code or authenticated transcript".into())
}

// ---- authenticated-transcript pin sidecar (P1-1) ---------------------------
//
// Every transcript this device authenticated leaves a `<pairing
// hex>.akdgm` sidecar in the journal directory, written before the
// `pending` journal entry so an entry always implies a pin. The journal
// store itself only reads `.akpair` files, so the sidecar never
// interferes with `load_all`. Crash recovery without the invite secret
// compares the re-sent `PairCommit` transcript against this pin instead
// of trusting the transcript's self-declared keys.

/// Version header of the authenticated-digest sidecar.
const DIGEST_SIDECAR_HEADER: &str = "AKDGM1";
/// Extension of the sidecar file (never `.akpair`, so the journal's
/// strict loader ignores it).
const DIGEST_SIDECAR_EXT: &str = "akdgm";
/// Distinguishes concurrent sidecar temp files within one process.
static SIDECAR_SEQ: AtomicU64 = AtomicU64::new(0);

/// Atomically persist the pin: an exclusively created (`create_new`)
/// unique temp file in the journal directory, fsync, then rename (the
/// same discipline as the `ak-store` journal writes). Exclusive
/// creation means a file or symlink planted at the temp name is never
/// written through; a crash at worst leaves the pin absent, which only
/// makes recovery refuse — never trust — the entry.
fn write_digest_sidecar(dir: &Path, pairing_id: &[u8; 16], digest: &[u8; 32]) -> io::Result<()> {
    let stem = hex(pairing_id);
    let seq = SIDECAR_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        "{stem}.{}.{seq}.{DIGEST_SIDECAR_EXT}.tmp",
        std::process::id()
    ));
    let final_path = dir.join(format!("{stem}.{DIGEST_SIDECAR_EXT}"));
    let text = format!(
        "{DIGEST_SIDECAR_HEADER}\npairing {}\ndigest {}\n",
        hex(pairing_id),
        hex_bytes(digest)
    );
    let write = || -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    };
    let result = write().and_then(|()| rename_over(&tmp, &final_path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Rename `tmp` over `final_path`; on a std/toolchain where rename
/// cannot replace an existing destination, remove it first (same
/// accepted trade-off as the journal store's `rename_replace`: the
/// remove-then-rename gap can at worst leave the pin absent, and an
/// absent pin only makes recovery refuse the entry).
fn rename_over(tmp: &Path, final_path: &Path) -> io::Result<()> {
    if fs::rename(tmp, final_path).is_ok() {
        return Ok(());
    }
    match fs::remove_file(final_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::rename(tmp, final_path)
}

/// Read a pairing's pin. `Ok(None)` = absent; a present-but-corrupt
/// sidecar (bad header, bad hex, wrong pairing id, trailing junk) is
/// `Err` — fail closed, never silently treat it as absent.
fn read_digest_sidecar(dir: &Path, pairing_id: &[u8; 16]) -> io::Result<Option<[u8; 32]>> {
    let path = dir.join(format!("{}.{}", hex(pairing_id), DIGEST_SIDECAR_EXT));
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let invalid = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("authenticated-transcript pin {what}"),
        )
    };
    let mut lines = text.lines();
    if lines.next() != Some(DIGEST_SIDECAR_HEADER) {
        return Err(invalid("has a bad header"));
    }
    let pairing = lines
        .next()
        .and_then(|l| l.strip_prefix("pairing "))
        .ok_or_else(|| invalid("is truncated"))?;
    let digest = lines
        .next()
        .and_then(|l| l.strip_prefix("digest "))
        .ok_or_else(|| invalid("is truncated"))?;
    if lines.next().is_some() {
        return Err(invalid("has trailing lines"));
    }
    let pairing = decode_hex_fixed::<16>(pairing).ok_or_else(|| invalid("has a bad pairing id"))?;
    if &pairing != pairing_id {
        return Err(invalid("names a different pairing"));
    }
    decode_hex_fixed::<32>(digest)
        .ok_or_else(|| invalid("has a bad digest"))
        .map(Some)
}

/// Lowercase hex of an arbitrary byte slice (the 16-byte [`hex`] is
/// pairing-id specific).
fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode exactly `N*2` lowercase-or-uppercase hex chars into fixed
/// bytes (mirror of the journal store's codec, kept local).
fn decode_hex_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (digit(pair[0])?) << 4 | (digit(pair[1])?);
    }
    Some(out)
}

/// Keyed-BLAKE3 MAC over `domain || transcript` — the exact construction
/// of `ak-identity::pairing_txn`'s private `proof_mac`, replicated so the
/// proofs this module builds verify under the frozen `verify_transcript`.
fn proof_mac(auth_key: &[u8; 32], domain: &str, transcript: &[u8; 256]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_keyed(auth_key);
    hasher.update(domain.as_bytes());
    hasher.update(transcript);
    *hasher.finalize().as_bytes()
}

/// MAC over the redeem request fields (endpoint-only; the server verifies
/// the Ed25519 signature over the same field set, never this MAC):
/// `"AK redeem request v1" || registry_id || lookup_digest ||
/// redeemer_device_id || request_id || redeemer_nonce || deadline_ms`.
pub fn redeem_request_mac(
    auth_key: &[u8; 32],
    registry_id: &[u8; 32],
    lookup_digest: &[u8; 32],
    redeemer_device_id: &[u8; 32],
    request_id: &[u8; 16],
    redeemer_nonce: &[u8; 32],
    deadline_ms: u64,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_keyed(auth_key);
    hasher.update(REDEEM_MAC_DOMAIN);
    hasher.update(registry_id);
    hasher.update(lookup_digest);
    hasher.update(redeemer_device_id);
    hasher.update(request_id);
    hasher.update(redeemer_nonce);
    hasher.update(&deadline_ms.to_be_bytes());
    *hasher.finalize().as_bytes()
}

/// Constant-time equality for 32-byte MAC material.
fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn wire_proof(proof: PairProofData) -> WireProof {
    WireProof {
        mac: proof.mac,
        signature: proof.signature,
    }
}

fn unwire_proof(proof: &WireProof) -> PairProofData {
    PairProofData {
        mac: proof.mac,
        signature: proof.signature,
    }
}

fn peer_of(transcript: &PairTranscript, me: [u8; 32]) -> [u8; 32] {
    if transcript.owner_device_id == me {
        transcript.redeemer_device_id
    } else {
        transcript.owner_device_id
    }
}

fn hex(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- plane waiting -----------------------------------------------------------

/// What a flow is taking frames off the plane for. The pairing trace names
/// both when the flow takes an error frame (Q-N3, review 21): a line that
/// says only which frame arrived does not place it in a flow.
#[derive(Debug, Clone, Copy)]
struct Waiting {
    /// `owner`, `redeemer` or `recovery`.
    flow: &'static str,
    /// The frame the flow was waiting for.
    frame: &'static str,
}

/// Wait until `extract` returns `Some` for a frame of `pairing_id`'s flow.
/// `Ok(None)` = timeout. Pairing-scoped `PairError` frames (deadline
/// ghosts, revoke/sweep aborts — including the all-zero object id the
/// ghost path uses) surface as unified flow errors.
///
/// Wave 30 (review 2026-11-07, R29-P2-01): the commit-time
/// `REDEEM_EXHAUSTED` refusal is CODE-scoped — it names the code, not the
/// losing pairing, and carries the all-zero request id — so it
/// cannot be matched the way pairing-scoped errors are. It is attributed
/// by asking the registry about OUR pairing: the server aborted the
/// losing transaction when it refused the commit, so a pairing that no
/// longer exists server-side was ours (fail promptly with the exhausted
/// refusal instead of waiting out the commit deadline); a pairing that
/// is still alive belongs to another flow and this one keeps waiting.
async fn wait_frame<T, F>(
    client: &RegistryClient,
    plane: &mut mpsc::Receiver<CtrlMessage>,
    pairing_id: [u8; 16],
    until_ms: u64,
    waiting: Waiting,
    extract: F,
) -> Result<Option<T>, PairFlowError>
where
    F: Fn(&CtrlMessage) -> Option<T>,
{
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(until_ms.saturating_sub(now_unix_ms()).max(1));
    loop {
        let frame = match tokio::time::timeout_at(deadline, plane.recv()).await {
            Err(_elapsed) => return Ok(None),
            Ok(Some(frame)) => frame,
            Ok(None) => return Err(PairFlowError::Client(ClientError::ConnectionLost)),
        };
        if let CtrlMessage::PairError {
            scope,
            object_id,
            code,
            ..
        } = &frame
        {
            if *scope == ak_signal::protocol_v2::ErrorScope::Pairing
                && (*object_id == pairing_id || *object_id == [0u8; 16])
            {
                // Q-N3: the error frame that ends this flow — the registry
                // logs its own decision; this line says when it was taken.
                // Review 21: by which flow, waiting for which frame, and
                // for which round. The all-zero id is any round's, so the
                // round waited for is named apart from it.
                pair_diag!(
                    "pairing error frame taken: code 0x{code:04x}, correlation {}, at {} ms on the clock; \
                     flow: {}, waiting for: {}, pairing {}",
                    if *object_id == pairing_id {
                        format!("this round (pairing {})", id_prefix(&pairing_id))
                    } else {
                        "all-zero".to_string()
                    },
                    now_unix_ms(),
                    waiting.flow,
                    waiting.frame,
                    id_prefix(&pairing_id)
                );
                return Err(PairFlowError::from_pair_code(*code));
            }
            if *scope == ak_signal::protocol_v2::ErrorScope::Code
                && *code == ak_signal::protocol_v2::message::REDEEM_EXHAUSTED
                && code_scoped_refusal_is_ours(client, pairing_id).await
            {
                // The frame names no pairing of its own; the status round
                // trip just above placed it in this one.
                pair_diag!(
                    "code-scoped error frame taken: code 0x{code:04x}, our pairing gone server-side, \
                     at {} ms on the clock; flow: {}, waiting for: {}, pairing {}",
                    now_unix_ms(),
                    waiting.flow,
                    waiting.frame,
                    id_prefix(&pairing_id)
                );
                return Err(PairFlowError::from_pair_code(*code));
            }
        }
        if let Some(value) = extract(&frame) {
            return Ok(Some(value));
        }
    }
}

/// Attribute one code-scoped `REDEEM_EXHAUSTED` refusal (R29-P2-01):
/// `true` when OUR pairing no longer exists server-side (the server
/// aborted it at the commit linearization point — the refusal was for
/// us); `false` when the transaction is still alive (the refusal
/// belonged to another flow of the same device) or the status round trip
/// itself failed for transport reasons (then the ordinary wait — and its
/// deadline — still bounds the flow).
async fn code_scoped_refusal_is_ours(client: &RegistryClient, pairing_id: [u8; 16]) -> bool {
    matches!(
        client.pair_status_query(pairing_id).await,
        Err(ClientError::Pair { .. })
    )
}

fn pairing_challenge_of(
    frame: &CtrlMessage,
    pairing_id: [u8; 16],
) -> Option<([u8; 256], WireProof)> {
    match frame {
        CtrlMessage::PairChallenge {
            transcript,
            owner_proof,
        } => {
            let embedded = crate::pairing::client::transcript_pairing_id(transcript);
            (embedded == pairing_id).then(|| (*transcript, owner_proof.clone()))
        }
        _ => None,
    }
}

fn pair_proof_of(frame: &CtrlMessage, pairing_id: [u8; 16], digest: [u8; 32]) -> Option<WireProof> {
    match frame {
        CtrlMessage::PairProof {
            pairing_id: pid,
            transcript_digest,
            redeemer_proof,
        } if *pid == pairing_id && *transcript_digest == digest => Some(redeemer_proof.clone()),
        _ => None,
    }
}

fn pair_commit_of(frame: &CtrlMessage, pairing_id: [u8; 16]) -> Option<CommitView> {
    match frame {
        CtrlMessage::PairCommit {
            transcript,
            owner_proof,
            redeemer_proof,
            commit_revision,
            ..
        } => {
            let embedded = crate::pairing::client::transcript_pairing_id(transcript);
            (embedded == pairing_id).then(|| CommitView {
                transcript: *transcript,
                owner_proof: owner_proof.clone(),
                redeemer_proof: redeemer_proof.clone(),
                commit_revision: *commit_revision,
            })
        }
        _ => None,
    }
}

/// Wait for `PairReady` of one pairing (digest `[0;32]` accepts any).
async fn wait_ready(
    client: &RegistryClient,
    plane: &mut mpsc::Receiver<CtrlMessage>,
    pairing_id: [u8; 16],
    digest: [u8; 32],
    wait_ms: u64,
    flow: &'static str,
) -> Result<(), PairFlowError> {
    let until = now_unix_ms().saturating_add(wait_ms);
    match wait_frame(
        client,
        plane,
        pairing_id,
        until,
        Waiting {
            flow,
            frame: "PairReady",
        },
        |frame| match frame {
            CtrlMessage::PairReady {
                pairing_id: pid,
                transcript_digest,
                ..
            } if *pid == pairing_id && (digest == [0u8; 32] || *transcript_digest == digest) => {
                Some(())
            }
            _ => None,
        },
    )
    .await?
    {
        Some(()) => Ok(()),
        None => Err(PairFlowError::Timeout),
    }
}

/// The steps of the finish after a verified commit, in the order they run.
/// The pairing trace names the last one that started when the finish stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinishStep {
    CommitPeer,
    MarkCommitted,
    SendApplied,
    WaitReady,
    MarkApplied,
}

impl FinishStep {
    fn name(self) -> &'static str {
        match self {
            FinishStep::CommitPeer => "commit_peer",
            FinishStep::MarkCommitted => "journal.mark_committed",
            FinishStep::SendApplied => "send PairApplied",
            FinishStep::WaitReady => "wait PairReady",
            FinishStep::MarkApplied => "journal.mark_applied",
        }
    }
}

/// The class of a flow failure for the pairing trace. Where the failure's
/// own text is fixed — a numeric code, a verification verdict, a transport
/// state — that text is the class. Where the text carries a free payload
/// only the class is given: a protocol message, a local-state message or an
/// I/O error can name a local path or a full pairing id, and a rejected
/// code names a symbol of what the user typed.
fn error_category(error: &PairFlowError) -> String {
    match error {
        PairFlowError::Client(ClientError::Protocol(_)) => "protocol violation".to_string(),
        PairFlowError::Client(inner) => inner.to_string(),
        PairFlowError::BadCode(_) => "invite code rejected".to_string(),
        PairFlowError::Unavailable { .. }
        | PairFlowError::Rejected { .. }
        | PairFlowError::Verify(_)
        | PairFlowError::Timeout => error.to_string(),
        PairFlowError::Local(_) => "local pairing state error".to_string(),
        PairFlowError::Io(inner) => match inner.raw_os_error() {
            Some(raw) => format!("local pairing store io error (os error {raw})"),
            None => format!("local pairing store io error ({:?})", inner.kind()),
        },
    }
}

fn map_client(err: ClientError) -> PairFlowError {
    match err {
        ClientError::Pair { code, .. } => PairFlowError::from_pair_code(code),
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_identity::pairing_txn::PairingVerifyError;

    // ---- P2: Debug redaction sentinel ------------------------------------

    #[test]
    fn created_invite_debug_never_leaks_the_bearer_code() {
        let code = InviteCode::generate();
        let invite = CreatedInvite {
            display: code.to_display(),
            hint: code.code_hint(),
            code_id: [9; 16],
            expires_ms: 0,
            revision: 4,
            lookup_digest: code.lookup_digest(),
        };
        let rendered = format!("{invite:?}");
        assert!(
            !rendered.contains(&invite.display),
            "Debug must not leak the bearer code: {rendered}"
        );
        assert!(rendered.contains("redacted"), "marks the field: {rendered}");
        assert!(
            rendered.contains(&invite.hint),
            "shows the mask: {rendered}"
        );
    }

    // ---- P1-1: pinned commit verification ---------------------------------

    fn commit_view_of(transcript: &PairTranscript, record: &PairingRecord) -> CommitView {
        CommitView {
            transcript: transcript.to_bytes(),
            owner_proof: WireProof {
                mac: record.owner_proof.mac,
                signature: record.owner_proof.signature,
            },
            redeemer_proof: WireProof {
                mac: record.redeemer_proof.mac,
                signature: record.redeemer_proof.signature,
            },
            commit_revision: 1,
        }
    }

    #[test]
    fn verify_commit_pinned_accepts_authentic_and_rejects_swapped_keys() {
        let owner = DeviceKeyPair::generate();
        let redeemer = DeviceKeyPair::generate();
        let code = InviteCode::generate();
        let transcript = PairTranscript {
            registry_id: [0x11; 32],
            code_id: [0x22; 16],
            code_revision: 1,
            pairing_id: [0x33; 16],
            request_id: [0x44; 16],
            lookup_digest: code.lookup_digest(),
            owner_device_id: owner.public_key(),
            redeemer_device_id: redeemer.public_key(),
            owner_nonce: [0x55; 32],
            redeemer_nonce: [0x66; 32],
            deadline_ms: 4_102_444_800_000, // 2100-01-01T00:00:00Z
        };
        let record = PairingRecord::build(transcript, &code.auth_key(), &owner, &redeemer);
        let pin = transcript.transcript_digest();

        // The authentic commit passes against its pin.
        let authentic = commit_view_of(&transcript, &record);
        assert!(verify_commit_pinned(&authentic, &pin).is_ok());

        // Malicious registry (P1-1): swap the owner device key to its own
        // and re-sign both proofs under the swapped keys. The MACs cannot
        // be forged without the code secret — and do not need to be, since
        // this path never checks them; the swapped signatures DO verify
        // under the keys the tampered transcript names, which is exactly
        // the self-attestation hole the pin closes.
        let attacker = DeviceKeyPair::generate();
        let mut swapped = transcript;
        swapped.owner_device_id = attacker.public_key();
        let bytes = swapped.to_bytes();
        let mac = [0u8; 32];
        let owner_signed = attacker.sign(&hello2::proof_signed_message(
            OWNER_PROOF_DOMAIN,
            &bytes,
            &mac,
        ));
        let redeemer_signed = attacker.sign(&hello2::proof_signed_message(
            REDEEMER_PROOF_DOMAIN,
            &bytes,
            &mac,
        ));
        assert!(
            DeviceKeyPair::verify(
                &swapped.owner_device_id,
                &hello2::proof_signed_message(OWNER_PROOF_DOMAIN, &bytes, &mac),
                &owner_signed
            ),
            "sanity: the forged signature self-attests — the old check passed this"
        );
        let forged = CommitView {
            transcript: bytes,
            owner_proof: WireProof {
                mac,
                signature: owner_signed,
            },
            redeemer_proof: WireProof {
                mac,
                signature: redeemer_signed,
            },
            commit_revision: 1,
        };
        let err = verify_commit_pinned(&forged, &pin).unwrap_err();
        assert!(
            matches!(
                err,
                PairFlowError::Verify(PairingVerifyError::TranscriptDigestMismatch)
            ),
            "swapped-key commit refused: {err:?}"
        );

        // The authentic commit against the WRONG pin is refused too: the
        // pin is the authority, not the commit's own structure.
        assert!(verify_commit_pinned(&authentic, &[0xEE; 32]).is_err());
    }

    // ---- Z-N7: a committed PairCommit processed after the deadline ----

    fn round_transcript(
        code: &InviteCode,
        owner: &DeviceKeyPair,
        redeemer: &DeviceKeyPair,
        deadline_ms: u64,
    ) -> PairTranscript {
        PairTranscript {
            registry_id: [0x11; 32],
            code_id: [0x22; 16],
            code_revision: 1,
            pairing_id: [0x33; 16],
            request_id: [0x44; 16],
            lookup_digest: code.lookup_digest(),
            owner_device_id: owner.public_key(),
            redeemer_device_id: redeemer.public_key(),
            owner_nonce: [0x55; 32],
            redeemer_nonce: [0x66; 32],
            deadline_ms,
        }
    }

    /// The grace bound is the same for waiting and for taking the frame,
    /// and the frame is bound to this round's transcript.
    #[test]
    fn live_commit_enforces_grace_and_expected_transcript() {
        // The contract's bound, written out here on purpose: the test must
        // fail if the product's grace moves, not move with it.
        const GRACE_MS: i64 = 10_000;
        let owner = DeviceKeyPair::generate();
        let redeemer = DeviceKeyPair::generate();
        let code = InviteCode::generate();
        let auth_key = code.auth_key();
        let deadline: i64 = 4_102_444_800_000; // 2100-01-01T00:00:00Z
        let transcript = round_transcript(&code, &owner, &redeemer, deadline as u64);
        let record = PairingRecord::build(transcript, &auth_key, &owner, &redeemer);
        let commit = commit_view_of(&transcript, &record);

        // This round's frame, taken anywhere inside the grace: the proof
        // check runs at the deadline, not at the (later) take time. Taken
        // past the grace: a timeout, not a proof failure.
        for (offset, outcome) in [
            (-1i64, "ok"),
            (0, "ok"),
            (1, "ok"),
            (GRACE_MS, "ok"),
            (GRACE_MS + 1, "timeout"),
        ] {
            let taken_at = (deadline + offset) as u64;
            let result = verify_live_commit(&commit, &transcript, &auth_key, taken_at).map(|_| ());
            match outcome {
                "ok" => assert!(result.is_ok(), "taken at D{offset:+} ms: {result:?}"),
                _ => assert!(
                    matches!(result, Err(PairFlowError::Timeout)),
                    "taken at D{offset:+} ms: {result:?}"
                ),
            }
        }

        // Another round of the same pairing id: validly signed under the
        // same keys and code, so the bare proof check accepts it on its own
        // terms. The live check refuses it inside the grace because it is
        // not this round's transcript, and past the grace as a timeout:
        // the bound is judged by this round's own deadline.
        let other = PairTranscript {
            owner_nonce: [0x77; 32],
            ..transcript
        };
        let other_record = PairingRecord::build(other, &auth_key, &owner, &redeemer);
        let other_commit = commit_view_of(&other, &other_record);
        assert!(
            verify_commit_full(&other_commit, &auth_key, (deadline - 1) as u64).is_ok(),
            "fixture: the bare proof check is self-consistent"
        );
        for offset in [-1i64, 0, 1, GRACE_MS] {
            let result = verify_live_commit(
                &other_commit,
                &transcript,
                &auth_key,
                (deadline + offset) as u64,
            )
            .map(|_| ());
            assert!(
                matches!(
                    result,
                    Err(PairFlowError::Verify(
                        PairingVerifyError::TranscriptDigestMismatch
                    ))
                ),
                "another round's transcript taken at D{offset:+} ms: {result:?}"
            );
        }
        let result = verify_live_commit(
            &other_commit,
            &transcript,
            &auth_key,
            (deadline + GRACE_MS + 1) as u64,
        )
        .map(|_| ());
        assert!(
            matches!(result, Err(PairFlowError::Timeout)),
            "past the grace the timeout comes before the transcript check: {result:?}"
        );

        // Inside the grace a broken MAC or signature is still refused.
        let mut bad_mac = commit_view_of(&transcript, &record);
        bad_mac.owner_proof.mac = [0xEE; 32];
        assert!(matches!(
            verify_live_commit(&bad_mac, &transcript, &auth_key, (deadline + 1) as u64).map(|_| ()),
            Err(PairFlowError::Verify(
                PairingVerifyError::InviteCodeMismatch
            ))
        ));
        let mut bad_signature = commit_view_of(&transcript, &record);
        bad_signature.redeemer_proof.signature = [0; 64];
        assert!(matches!(
            verify_live_commit(
                &bad_signature,
                &transcript,
                &auth_key,
                (deadline + 1) as u64
            )
            .map(|_| ()),
            Err(PairFlowError::Verify(PairingVerifyError::PublicKeyMismatch))
        ));
    }

    /// What the joiner is doing, without waiting for it: how its task ended
    /// when it already finished (a panic or a cancellation included, so the
    /// caller's own reason is never lost), else that it is still running.
    async fn joiner_state<T>(
        redeeming: &mut tokio::task::JoinHandle<Result<T, PairFlowError>>,
    ) -> String {
        if !redeeming.is_finished() {
            return "the joiner is still running".to_string();
        }
        let joined = redeeming.await.map(|inner| inner.map(|_| ()));
        format!("the joiner task ended with {joined:?}")
    }

    /// The one `InviteRedeemOffered` the owner loop would answer, or a
    /// failure that names why it never came and what the joiner was doing:
    /// it gave up first, the plane closed, or `limit` ran out.
    async fn offered_redeem<T>(
        plane: &mut mpsc::Receiver<CtrlMessage>,
        redeeming: &mut tokio::task::JoinHandle<Result<T, PairFlowError>>,
        limit: Duration,
    ) -> (
        [u8; 16],
        u64,
        [u8; 16],
        [u8; 32],
        ak_signal::protocol_v2::RedeemRequest,
    ) {
        let give_up_at = tokio::time::Instant::now() + limit;
        loop {
            tokio::select! {
                joined = &mut *redeeming => {
                    let joined = joined.map(|inner| inner.map(|_| ()));
                    panic!("the joiner finished before the owner was offered anything: {joined:?}")
                }
                frame = plane.recv() => match frame {
                    Some(CtrlMessage::InviteRedeemOffered {
                        code_id,
                        code_revision,
                        pairing_id,
                        redeemer_device_id,
                        redeem,
                    }) => {
                        return (code_id, code_revision, pairing_id, redeemer_device_id, redeem);
                    }
                    Some(_) => continue,
                    None => panic!(
                        "owner pairing plane closed before the offer; {}",
                        joiner_state(redeeming).await
                    ),
                },
                _ = tokio::time::sleep_until(give_up_at) => {
                    panic!(
                        "no InviteRedeemOffered within {limit:?}; {}",
                        joiner_state(redeeming).await
                    );
                }
            }
        }
    }

    async fn orchestrator_on(
        registry: &registry::TestRegistry,
        name: &str,
        key: &DeviceKeyPair,
        dir: &Path,
        config: OrchestratorConfig,
    ) -> PairingOrchestrator {
        let client = RegistryClient::connect(registry.addr, registry.registry_id, key, name)
            .await
            .expect("connect to the registry");
        PairingOrchestrator::open(client, key.clone(), dir, config).expect("open")
    }

    /// The registry commits at or before the deadline; both endpoints take
    /// the `PairCommit` after it, inside the delivery grace, and both must
    /// still end at `Applied` with the other device booked (Z-N7).
    ///
    /// The ordering does not depend on how fast the machine is. The real
    /// registry runs with a generous window; the two flows park their
    /// frames; the test checks that they arrived while the wall clock was
    /// still before the deadline; and only the moment each flow takes its
    /// frame is then pinned, on these two instances alone.
    #[tokio::test]
    async fn live_commit_delivered_after_deadline_applies_on_both_sides() {
        const LIMIT: Duration = Duration::from_secs(20);
        // The contract's delivery grace, written out on purpose (see the
        // table test).
        const GRACE_MS: u64 = 10_000;
        const TAKEN_PAST_DEADLINE_MS: u64 = 1;
        let registry = registry::spawn().await;

        let owner_endpoint_bytes = br#"{"addrs":["127.0.0.1:7100"]}"#.to_vec();
        let joiner_endpoint =
            ak_signal::protocol_v2::EndpointBlob(br#"{"addrs":["127.0.0.1:7101"]}"#.to_vec());

        let a_key = DeviceKeyPair::generate();
        let a_dir = tempfile::tempdir().unwrap();
        let a = orchestrator_on(&registry, "alice", &a_key, a_dir.path(), test_config()).await;
        a.client().arm_commit_gate();
        let mut a_plane = a.client().subscribe_plane();
        // A one-shot task code, as in the flow that failed on CI.
        let invite = a
            .create_task_invite(
                [0x51; 16],
                0,
                ak_signal::protocol_v2::EndpointBlob(owner_endpoint_bytes.clone()),
            )
            .await
            .expect("claim the one-shot task code");

        let b_key = DeviceKeyPair::generate();
        let b_dir = tempfile::tempdir().unwrap();
        let b = orchestrator_on(&registry, "bob", &b_key, b_dir.path(), test_config()).await;
        b.client().arm_commit_gate();

        let code_display = invite.display.clone();
        let mut redeeming = {
            let b = b.clone();
            tokio::spawn(async move { b.redeem_task(&code_display, joiner_endpoint).await })
        };
        let (code_id, code_revision, pairing_id, redeemer_device_id, redeem) =
            offered_redeem(&mut a_plane, &mut redeeming, LIMIT).await;
        let deadline = redeem.deadline_ms;
        let answering = {
            let a = a.clone();
            tokio::spawn(async move {
                a.owner_answer_offer(
                    code_id,
                    code_revision,
                    pairing_id,
                    redeemer_device_id,
                    redeem,
                )
                .await
            })
        };

        // Both frames parked, one per side, both of this pairing. The
        // registry sends a `PairCommit` only once its commit succeeded, and
        // both arrived while the wall clock was still before the deadline:
        // in this run the registry had committed by then. (This is an
        // observation about this run, not something an endpoint can tell
        // from a frame that arrives late.)
        let parked = tokio::time::timeout(LIMIT, async {
            a.client().commit_parked_within(LIMIT).await
                && b.client().commit_parked_within(LIMIT).await
        })
        .await;
        assert_eq!(
            parked,
            Ok(true),
            "both sides take the registry's PairCommit"
        );
        assert_eq!(a.client().parked_commits(), vec![pairing_id]);
        assert_eq!(b.client().parked_commits(), vec![pairing_id]);
        let parked_at = now_unix_ms();
        assert!(
            parked_at < deadline,
            "fixture: both frames arrived at {parked_at}, before the deadline {deadline}"
        );
        assert!(!a_dir.path().join(BOOK_NAME).exists());
        assert!(!b_dir.path().join(BOOK_NAME).exists());

        // Each flow now takes its frame 1 ms past the deadline, well inside
        // the grace.
        let taken_at = deadline + TAKEN_PAST_DEADLINE_MS;
        assert!(taken_at <= deadline + GRACE_MS);
        a.client().pin_delivery_now(taken_at);
        b.client().pin_delivery_now(taken_at);
        assert_eq!(a.client().frame_taken_at_unix_ms(), taken_at);
        assert_eq!(b.client().frame_taken_at_unix_ms(), taken_at);
        assert_eq!(a.client().release_commits(), 1);
        assert_eq!(b.client().release_commits(), 1);

        let (owner, redeemer) =
            tokio::time::timeout(LIMIT, async { tokio::join!(answering, redeeming) })
                .await
                .expect("both flows finish after the late commit");
        let owner = owner
            .expect("owner task")
            .expect("owner accepts the commit the registry made before the deadline");
        let redeemer = redeemer
            .expect("redeemer task")
            .expect("redeemer accepts the commit the registry made before the deadline");
        assert_eq!(owner.record().transcript.pairing_id, pairing_id);
        assert_eq!(redeemer.verified.record().transcript.pairing_id, pairing_id);
        assert_eq!(
            owner.record().transcript.transcript_digest(),
            redeemer.verified.record().transcript.transcript_digest()
        );
        // The joiner also got the owner's endpoint off the late frame.
        assert_eq!(redeemer.owner_endpoint, Some(owner_endpoint_bytes));

        // Each side booked the other, and both journals end at Applied.
        let a_book = fs::read_to_string(a_dir.path().join(BOOK_NAME)).expect("owner trust book");
        let b_book = fs::read_to_string(b_dir.path().join(BOOK_NAME)).expect("redeemer trust book");
        assert!(a_book.contains(&Fingerprint::from_public_key(&b_key.public_key()).to_hex()));
        assert!(b_book.contains(&Fingerprint::from_public_key(&a_key.public_key()).to_hex()));
        for dir in [a_dir.path(), b_dir.path()] {
            let records = PairingJournal::open(&dir.join(JOURNAL_SUBDIR))
                .unwrap()
                .load_all()
                .unwrap();
            assert!(
                records.iter().any(|record| {
                    record.pairing_id == pairing_id && record.phase == PairingPhase::Applied
                }),
                "journal of {} is Applied for this pairing",
                dir.display()
            );
        }
    }

    // ---- P1-1: pin sidecar persistence ------------------------------------

    #[test]
    fn digest_sidecar_roundtrip_is_strict_and_journal_ignores_it() {
        let dir = tempfile::tempdir().unwrap();
        let journal = PairingJournal::open(dir.path()).unwrap();
        let pairing_id = [0x71; 16];
        let digest = [0xD3; 32];

        assert_eq!(read_digest_sidecar(dir.path(), &pairing_id).unwrap(), None);
        write_digest_sidecar(dir.path(), &pairing_id, &digest).unwrap();
        assert_eq!(
            read_digest_sidecar(dir.path(), &pairing_id).unwrap(),
            Some(digest)
        );
        // Atomic replace: same value, still exactly one sidecar file.
        write_digest_sidecar(dir.path(), &pairing_id, &digest).unwrap();
        assert_eq!(
            read_digest_sidecar(dir.path(), &pairing_id).unwrap(),
            Some(digest)
        );
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "no temp residue: {names:?}");
        assert!(names[0].ends_with(".akdgm"));

        // The strict journal store coexists with the sidecar.
        assert!(journal.load_all().unwrap().is_empty());
        // A different pairing has its own slot.
        assert_eq!(read_digest_sidecar(dir.path(), &[0x72; 16]).unwrap(), None);

        // Corrupt or foreign variants fail closed, never read as absent.
        let stem = hex(&pairing_id);
        let other = hex(&[0x72; 16]);
        let digest_hex = hex_bytes(&digest);
        let path = dir.path().join(format!("{stem}.{DIGEST_SIDECAR_EXT}"));
        let bad_variants = [
            String::from(""),
            format!("{DIGEST_SIDECAR_HEADER}\n"),
            format!("{DIGEST_SIDECAR_HEADER}\npairing zzzz\ndigest {digest_hex}\n"),
            format!("{DIGEST_SIDECAR_HEADER}\npairing {other}\ndigest {digest_hex}\n"),
            format!("{DIGEST_SIDECAR_HEADER}\npairing {stem}\ndigest zzzz\n"),
            format!("{DIGEST_SIDECAR_HEADER}\npairing {stem}\ndigest {digest_hex}\nextra\n"),
            format!("AKDGM2\npairing {stem}\ndigest {digest_hex}\n"),
        ];
        for text in bad_variants {
            fs::write(&path, text.as_bytes()).unwrap();
            assert!(
                read_digest_sidecar(dir.path(), &pairing_id).is_err(),
                "must fail closed for {text:?}"
            );
        }
    }

    // ---- P1-1: recovery over the real registry -----------------------------

    mod registry {
        use super::super::*;
        use ak_signal::server_v2::engine::serve_v2;
        use ak_signal::server_v2::store::RegistryStore;
        use ak_signal::server_v2::Limits;
        use tokio::net::TcpListener;
        use tokio::sync::watch;

        pub(super) struct TestRegistry {
            pub addr: std::net::SocketAddr,
            pub registry_id: [u8; 32],
            shutdown: watch::Sender<bool>,
        }

        impl Drop for TestRegistry {
            fn drop(&mut self) {
                let _ = self.shutdown.send(true);
            }
        }

        pub(super) async fn spawn() -> TestRegistry {
            let registry_key = DeviceKeyPair::generate();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind v2 port");
            let addr = listener.local_addr().expect("local addr");
            let store = RegistryStore::open_in_memory().expect("open registry store");
            let (shutdown, shutdown_rx) = watch::channel(false);
            let key = registry_key.clone();
            tokio::spawn(async move {
                let _ = serve_v2(listener, &key, store, Limits::default(), shutdown_rx).await;
            });
            TestRegistry {
                addr,
                registry_id: registry_key.public_key(),
                shutdown,
            }
        }
    }

    fn test_config() -> OrchestratorConfig {
        OrchestratorConfig {
            redeem_window_ms: 30_000,
            ready_wait_ms: 30_000,
            claim_conflict_retries: 8,
        }
    }

    async fn connected_orchestrator(
        registry: &registry::TestRegistry,
        name: &str,
        key: &DeviceKeyPair,
        dir: &Path,
    ) -> PairingOrchestrator {
        let client = RegistryClient::connect(registry.addr, registry.registry_id, key, name)
            .await
            .expect("connect to the registry");
        PairingOrchestrator::open(client, key.clone(), dir, test_config()).expect("open")
    }

    /// Alice invites, her owner loop runs, Bob redeems up to the verified
    /// commit, then "crashes" (connection closed, orchestrator dropped)
    /// before any local apply — the checkpoint recovery resumes from.
    async fn redeem_to_commit_then_crash(
        registry: &registry::TestRegistry,
        b_dir: &Path,
        b_key: &DeviceKeyPair,
    ) -> CommittedPairing {
        let a_dir = tempfile::tempdir().unwrap();
        let a_key = DeviceKeyPair::generate();
        let a = connected_orchestrator(registry, "alice", &a_key, a_dir.path()).await;
        let invite = a.create_invite(0).await.expect("claim invite");
        a.spawn_owner_loop();

        let b = connected_orchestrator(registry, "bob", b_key, b_dir).await;
        let committed = b
            .redeem_to_commit(&invite.display)
            .await
            .expect("redeem to commit");
        b.client().close().await;
        drop(b);
        committed
    }

    #[tokio::test]
    async fn crashed_redeemer_recovers_without_code_via_the_pinned_transcript() {
        let registry = registry::spawn().await;
        let b_key = DeviceKeyPair::generate();
        let b_dir = tempfile::tempdir().unwrap();
        let journal_dir = b_dir.path().join(JOURNAL_SUBDIR);
        let committed = redeem_to_commit_then_crash(&registry, b_dir.path(), &b_key).await;

        // The pin landed before the crash window and matches the commit's
        // transcript exactly.
        let pin =
            read_digest_sidecar(&journal_dir, &committed.pairing_id).expect("pin sidecar readable");
        assert_eq!(
            pin.as_ref(),
            Some(&committed.verified.record().transcript.transcript_digest())
        );

        // Restart WITHOUT the invite code: recovery must succeed purely on
        // the local pin.
        let b2 = connected_orchestrator(&registry, "bob", &b_key, b_dir.path()).await;
        let applied = b2
            .recover(&[])
            .await
            .expect("recovery via the pinned digest");
        assert_eq!(applied, vec![committed.pairing_id]);

        let records = PairingJournal::open(&journal_dir)
            .unwrap()
            .load_all()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].phase, PairingPhase::Applied);
        assert!(b_dir.path().join(BOOK_NAME).is_file(), "trust book written");

        // Idempotent: nothing left to recover.
        let again = b2.recover(&[]).await.expect("second recovery");
        assert!(again.is_empty(), "{again:?}");
    }

    #[tokio::test]
    async fn recovery_without_code_or_pin_is_refused_not_degraded() {
        let registry = registry::spawn().await;
        let b_key = DeviceKeyPair::generate();
        let b_dir = tempfile::tempdir().unwrap();
        let journal_dir = b_dir.path().join(JOURNAL_SUBDIR);
        let committed = redeem_to_commit_then_crash(&registry, b_dir.path(), &b_key).await;

        // Simulate a pre-pin journal (P1-1): the entry exists, but this
        // device never persisted an authenticated-transcript digest and
        // the operator supplies no code. The server's re-sent commit —
        // honest here, hostile in the attack — must NOT be trusted.
        let pin_path = journal_dir.join(format!(
            "{}.{}",
            hex(&committed.pairing_id),
            DIGEST_SIDECAR_EXT
        ));
        fs::remove_file(&pin_path).expect("strip the pin");

        let b2 = connected_orchestrator(&registry, "bob", &b_key, b_dir.path()).await;
        let err = b2
            .recover(&[])
            .await
            .expect_err("no code and no pin must be refused");
        match &err {
            PairFlowError::Local(msg) => assert_eq!(
                msg,
                "cannot verify without invite code or authenticated transcript"
            ),
            other => panic!("expected the explicit refusal, got {other:?}"),
        }

        // Nothing was committed locally: the entry stays pending and the
        // trust book was never written.
        let records = PairingJournal::open(&journal_dir)
            .unwrap()
            .load_all()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].phase, PairingPhase::Pending);
        assert!(!b_dir.path().join(BOOK_NAME).exists());
    }

    // ---- Q-N3, review 21: the pairing trace of the flows --------------------
    //
    // Each test here runs its body in a child copy of this executable with
    // the trace on (the switch is process-wide — see `pairing::diag_test`)
    // and against the real registry: the lines are only worth reading next
    // to real frames. Every scenario ends on what stands locally — the
    // journal, the trust book, the error returned — as well as on the lines.
    mod diag {
        use super::super::*;
        use super::{offered_redeem, orchestrator_on, registry, test_config};
        use crate::pairing::client::{ClientConfig, RegistryClient};
        use crate::pairing::diag_test::{self, Switch, Traced};
        use ak_signal::protocol_v2::message::REDEEM_EXHAUSTED;
        use ak_signal::protocol_v2::{CtrlFrame, ErrorScope, PAIR_UNAVAILABLE};

        const LIMIT: Duration = Duration::from_secs(20);

        const TWO_SUBSCRIBERS: &str = "pairing::orchestrator::tests::diag::a_frame_only_another_subscriber_took_is_not_reported_as_taken";
        const TASK_READY_ERROR: &str = "pairing::orchestrator::tests::diag::a_task_join_stops_waiting_for_ready_on_this_round_s_error";
        const PLAIN_READY_ERROR: &str = "pairing::orchestrator::tests::diag::a_plain_redeem_stops_waiting_for_ready_on_this_round_s_error";
        const APPLIED_OFFLINE: &str = "pairing::orchestrator::tests::diag::a_joiner_stops_sending_pair_applied_without_a_connection";
        const BOOK_UNWRITABLE: &str = "pairing::orchestrator::tests::diag::a_joiner_stops_at_commit_peer_when_the_book_cannot_be_written";
        const STALE_ENTRY: &str = "pairing::orchestrator::tests::diag::a_joiner_stops_at_mark_committed_on_an_entry_already_applied";
        const ENTRY_BLOCKED: &str = "pairing::orchestrator::tests::diag::a_joiner_stops_at_mark_applied_when_the_entry_cannot_be_replaced";
        const SUCCESS: &str =
            "pairing::orchestrator::tests::diag::a_paired_flow_writes_no_stop_line";
        const BEFORE_COMMIT: &str = "pairing::orchestrator::tests::diag::a_stop_before_the_commit_writes_one_line_without_a_stage";
        const OWNER_STOP: &str = "pairing::orchestrator::tests::diag::an_owner_stop_after_the_commit_writes_one_owner_line";
        const ONESHOT: &str =
            "pairing::orchestrator::tests::diag::a_redeem_refused_at_its_answer_is_taken_on_the_oneshot";

        /// Which joiner entry point a scenario drives.
        #[derive(Clone, Copy, Debug)]
        enum Entry {
            /// `redeem`: the daemon of the admission case joins this way.
            Plain,
            /// `redeem_task`: the task layer joins this way.
            Task,
        }

        /// Both flows of one one-shot task code where the Commit fixture of
        /// `live_commit_delivered_after_deadline_applies_on_both_sides`
        /// stands: the registry has committed, each side's `PairCommit` is
        /// parked before its dispatch, and so nothing local has run after
        /// the commit on either side.
        struct Parked {
            owner: PairingOrchestrator,
            owner_key: DeviceKeyPair,
            owner_dir: tempfile::TempDir,
            joiner: PairingOrchestrator,
            joiner_dir: tempfile::TempDir,
            /// The code the joiner typed — it must never reach a line.
            code: String,
            /// Still waiting for its commit.
            redeeming: tokio::task::JoinHandle<Result<VerifiedPairing, PairFlowError>>,
            /// Still waiting for its commit.
            answering: tokio::task::JoinHandle<Result<VerifiedPairing, PairFlowError>>,
            pairing_id: [u8; 16],
        }

        /// Brings both flows of one one-shot task code to where [`Parked`]
        /// says. `traced` is the trace the child captured: a pairing that
        /// stops on its way to the commit says in it where.
        async fn parked_at_the_commit(
            registry: &registry::TestRegistry,
            entry: Entry,
            joiner_config: OrchestratorConfig,
            traced: &Traced,
        ) -> Parked {
            let owner_endpoint =
                ak_signal::protocol_v2::EndpointBlob(br#"{"addrs":["127.0.0.1:7100"]}"#.to_vec());
            let joiner_endpoint =
                ak_signal::protocol_v2::EndpointBlob(br#"{"addrs":["127.0.0.1:7101"]}"#.to_vec());
            let owner_key = DeviceKeyPair::generate();
            let owner_dir = tempfile::tempdir().unwrap();
            let owner = orchestrator_on(
                registry,
                "alice",
                &owner_key,
                owner_dir.path(),
                test_config(),
            )
            .await;
            owner.client().arm_commit_gate();
            let mut owner_plane = owner.client().subscribe_plane();
            let invite = owner
                .create_task_invite([0x51; 16], 0, owner_endpoint)
                .await
                .expect("claim the one-shot task code");

            let joiner_key = DeviceKeyPair::generate();
            let joiner_dir = tempfile::tempdir().unwrap();
            let joiner = orchestrator_on(
                registry,
                "bob",
                &joiner_key,
                joiner_dir.path(),
                joiner_config,
            )
            .await;
            joiner.client().arm_commit_gate();

            let code = invite.display.clone();
            let mut redeeming = {
                let joiner = joiner.clone();
                tokio::spawn(async move {
                    match entry {
                        Entry::Plain => joiner.redeem(&code).await,
                        Entry::Task => joiner
                            .redeem_task(&code, joiner_endpoint)
                            .await
                            .map(|task| task.verified),
                    }
                })
            };
            let (code_id, code_revision, pairing_id, redeemer_device_id, redeem) =
                offered_redeem(&mut owner_plane, &mut redeeming, LIMIT).await;
            let deadline = redeem.deadline_ms;
            let answering = {
                let owner = owner.clone();
                tokio::spawn(async move {
                    owner
                        .owner_answer_offer(
                            code_id,
                            code_revision,
                            pairing_id,
                            redeemer_device_id,
                            redeem,
                        )
                        .await
                })
            };

            let parked = tokio::time::timeout(LIMIT, async {
                owner.client().commit_parked_within(LIMIT).await
                    && joiner.client().commit_parked_within(LIMIT).await
            })
            .await;
            assert!(
                parked == Ok(true),
                "both sides parked the registry's PairCommit ({parked:?}); the trace so far:\n{}\n\
                 what the registry and the clients said:\n{}",
                traced.lines().join("\n"),
                traced.others().join("\n")
            );
            assert_eq!(owner.client().parked_commits(), vec![pairing_id]);
            assert_eq!(joiner.client().parked_commits(), vec![pairing_id]);
            assert!(
                now_unix_ms() < deadline,
                "fixture: both frames arrived before the deadline {deadline}"
            );
            Parked {
                owner,
                owner_key,
                owner_dir,
                joiner,
                joiner_dir,
                code: invite.display,
                redeeming,
                answering,
                pairing_id,
            }
        }

        fn ready(pairing_id: [u8; 16]) -> CtrlMessage {
            CtrlMessage::PairReady {
                pairing_id,
                transcript_digest: [0x50; 32],
                commit_revision: 1,
                owner_applied_signature: [0; 64],
                redeemer_applied_signature: [0; 64],
            }
        }

        /// A pairing-scoped `unavailable` naming `object_id`, as the
        /// registry answers an applied confirmation it refuses.
        fn unavailable(object_id: [u8; 16]) -> CtrlMessage {
            CtrlMessage::PairError {
                request_id: [0; 16],
                scope: ErrorScope::Pairing,
                object_id,
                code: PAIR_UNAVAILABLE,
                retry_after_ms: 0,
            }
        }

        fn entry_of(dir: &Path, pairing_id: &[u8; 16]) -> PathBuf {
            dir.join(JOURNAL_SUBDIR)
                .join(format!("{}.akpair", hex(pairing_id)))
        }

        /// Every journal entry of an identity directory with its phase.
        fn phases(dir: &Path) -> Vec<([u8; 16], PairingPhase)> {
            PairingJournal::open(&dir.join(JOURNAL_SUBDIR))
                .expect("open the journal")
                .load_all()
                .expect("load the journal")
                .into_iter()
                .map(|record| (record.pairing_id, record.phase))
                .collect()
        }

        /// Whether the journal already shows `pairing_id` at `phase`. An
        /// entry that is being replaced just now reads as not there yet.
        fn journal_has(dir: &Path, pairing_id: [u8; 16], phase: PairingPhase) -> bool {
            PairingJournal::open(&dir.join(JOURNAL_SUBDIR))
                .and_then(|journal| journal.load_all())
                .map(|records| {
                    records
                        .iter()
                        .any(|record| record.pairing_id == pairing_id && record.phase == phase)
                })
                .unwrap_or(false)
        }

        async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
            let give_up_at = Instant::now() + LIMIT;
            while !cond() {
                assert!(
                    Instant::now() < give_up_at,
                    "{what} did not happen within {LIMIT:?}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        /// The plane wait of one redeemer flow for this round's commit,
        /// bounded to `ms`; `Ok(true)` once it took the commit.
        async fn wait_for_the_commit(
            client: &RegistryClient,
            flow: &mut mpsc::Receiver<CtrlMessage>,
            pairing_id: [u8; 16],
            ms: u64,
        ) -> Result<bool, PairFlowError> {
            wait_frame(
                client,
                flow,
                pairing_id,
                now_unix_ms().saturating_add(ms),
                Waiting {
                    flow: "redeemer",
                    frame: "PairCommit",
                },
                |frame| pair_commit_of(frame, pairing_id),
            )
            .await
            .map(|commit| commit.is_some())
        }

        /// A closing line reads `{head}{elapsed}{middle}{now} ms on the clock{tail}`:
        /// both readings are whole milliseconds, nothing else stands between.
        fn closing_line(line: &str, head: &str, middle: &str, tail: &str) {
            let Some(rest) = line.strip_prefix(head) else {
                panic!("the line does not start with {head:?}: {line}")
            };
            let Some(rest) = rest.strip_suffix(tail) else {
                panic!("the line does not end with {tail:?}: {line}")
            };
            let Some((elapsed, now)) = rest.split_once(middle) else {
                panic!("no {middle:?} in {line}")
            };
            let Some(now) = now.strip_suffix(" ms on the clock") else {
                panic!("no clock reading in {line}")
            };
            let milliseconds =
                |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
            assert!(
                milliseconds(elapsed) && milliseconds(now),
                "two millisecond readings, got {elapsed:?} and {now:?}: {line}"
            );
        }

        /// The joiner's one closing line: this failure after the commit,
        /// stopped at this stage, for this round.
        fn one_joiner_stop(traced: &Traced, category: &str, stage: &str, prefix: &str) {
            let stopped = traced.the_one("redeemer: flow stopped with");
            closing_line(
                &stopped,
                &format!("redeemer: flow stopped with `{category}` "),
                " ms after it began, at ",
                &format!("; after the commit, last stage started: {stage}, pairing {prefix}"),
            );
        }

        /// No line carries the full pairing id or the code a device typed.
        fn nothing_private(traced: &Traced, pairing_id: &[u8; 16], code: &str) {
            for line in traced.lines() {
                assert!(
                    !line.contains(&hex(pairing_id)),
                    "a full pairing id: {line}"
                );
                assert!(!line.contains(code), "the invite code: {line}");
            }
        }

        /// The controlled local write failure of the `applied` mark.
        /// Windows refuses to replace a file that is open without
        /// share-delete: the flow still loads the committed entry, the
        /// write of `applied` fails, and the committed entry stays.
        #[cfg(windows)]
        fn hold_entry_against_replacement(entry: &Path) -> std::fs::File {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(entry)
                .expect("hold the entry open")
        }

        /// The joiner takes its commit and runs its half up to `PairApplied`.
        /// The registry's applied handler answers `unavailable` on several
        /// of its exits (nothing to apply, a state past committing, a
        /// signature it refuses), pairing-scoped and naming this round; the
        /// joiner takes that frame while it waits for `PairReady`.
        async fn stops_waiting_for_ready(entry: Entry, traced: &Traced) {
            let registry = registry::spawn().await;
            let parked = parked_at_the_commit(&registry, entry, test_config(), traced).await;
            let Parked {
                owner,
                owner_key,
                joiner,
                redeeming,
                answering,
                joiner_dir,
                code,
                pairing_id,
                ..
            } = parked;
            let prefix = id_prefix(&pairing_id);

            // The commit goes into the joiner's stream first, the error
            // right behind it.
            assert_eq!(joiner.client().release_commits(), 1);
            joiner.client().inject(unavailable(pairing_id));

            let error = redeeming
                .await
                .expect("the joiner task")
                .expect_err("the joiner stops on this round's error");
            assert!(
                matches!(error, PairFlowError::Unavailable { code } if code == PAIR_UNAVAILABLE),
                "the original error comes back, got `{}`",
                error_category(&error)
            );

            // Taken by the flow, where it waited; then the one closing line.
            let taken = traced.the_one("pairing error frame taken");
            assert!(
                taken.starts_with(&format!(
                    "pairing error frame taken: code 0x2101, correlation this round (pairing {prefix}), at "
                )),
                "{taken}"
            );
            assert!(
                taken.ends_with(&format!(
                    "; flow: redeemer, waiting for: PairReady, pairing {prefix}"
                )),
                "{taken}"
            );
            one_joiner_stop(
                traced,
                "pairing target unavailable (0x2101)",
                "wait PairReady",
                &prefix,
            );
            assert_eq!(
                traced.count("flow stopped with"),
                1,
                "one line for this flow"
            );
            assert!(traced.before("pairing error frame taken", "redeemer: flow stopped with"));

            // What stands locally: the owner is booked and the journal says
            // committed. No `PairReady` came, so nothing marked it applied.
            let book = fs::read_to_string(joiner_dir.path().join(BOOK_NAME))
                .expect("the joiner booked the owner");
            assert!(
                book.contains(&Fingerprint::from_public_key(&owner_key.public_key()).to_hex()),
                "the owner's fingerprint is in the joiner's book"
            );
            assert_eq!(
                phases(joiner_dir.path()),
                vec![(pairing_id, PairingPhase::Committed)]
            );

            // The owner is still parked on its commit. Closing its plane
            // ends its flow BEFORE the commit: one owner line, of the shape
            // without a stage — and the joiner's line is not written again.
            owner.client().close().await;
            let owner_error = answering
                .await
                .expect("the owner task")
                .expect_err("the owner's plane closed");
            assert!(
                matches!(
                    owner_error,
                    PairFlowError::Client(ClientError::ConnectionLost)
                ),
                "the owner's own error, got `{}`",
                error_category(&owner_error)
            );
            closing_line(
                &traced.the_one("owner: flow stopped with"),
                "owner: flow stopped with `connection lost while waiting for the reply` ",
                " ms after the offer, at ",
                "",
            );
            assert_eq!(
                traced.count("flow stopped with"),
                2,
                "one line per flow, none twice"
            );
            nothing_private(traced, &pairing_id, &code);
        }

        /// Review 21: "at least one delivery succeeded" is not "the flow the
        /// frame was meant for took it". Two plane subscribers; the flow's
        /// own stream is full, another flow's has room. This round's error
        /// is queued to one of the two, the flow waits its bound out and
        /// takes nothing — and the trace says exactly that, with no "taken"
        /// line. The control repeats it with room in the flow's stream: two
        /// of two queued, and only then a line says the flow took the frame.
        ///
        /// Goes red when the dispatch line sums the destinations up again
        /// (one flag for "queued"), when a merely queued frame is reported
        /// as taken, or — in the control — when a frame the flow did take
        /// is not reported as taken, with its flow, step and round.
        #[tokio::test]
        async fn a_frame_only_another_subscriber_took_is_not_reported_as_taken() {
            let Some(traced) = diag_test::traced_process(TWO_SUBSCRIBERS, Switch::On) else {
                return;
            };
            let pairing = [0x21; 16];

            // Both subscribers hold a frame of another round; only the
            // other flow's is read, so only it has room again.
            let client = RegistryClient::offline(ClientConfig {
                plane_capacity: 1,
                ..ClientConfig::default()
            });
            let mut flow = client.subscribe_plane();
            let mut other = client.subscribe_plane();
            client
                .deliver_as_reader(1, vec![CtrlFrame::new(ready([0x31; 16]))])
                .await;
            assert!(
                matches!(other.recv().await, Some(CtrlMessage::PairReady { pairing_id, .. }) if pairing_id == [0x31; 16]),
                "the other flow took the earlier frame"
            );
            client
                .deliver_as_reader(2, vec![CtrlFrame::new(unavailable(pairing))])
                .await;
            let taken = wait_for_the_commit(&client, &mut flow, pairing, 300).await;
            assert!(
                matches!(taken, Ok(false)),
                "the flow waited out its bound and took nothing: {taken:?}"
            );
            assert!(
                matches!(other.recv().await, Some(CtrlMessage::PairError { object_id, code, .. }) if object_id == pairing && code == PAIR_UNAVAILABLE),
                "the other flow did take the error"
            );
            assert_eq!(traced.lines().len(), 2, "{}", traced.lines().join("\n"));
            assert_eq!(
                traced.count("taken"),
                0,
                "queued to another flow is not taken by this one"
            );
            assert_eq!(
                traced.the_one("registry frame #1: pairing error 0x2101"),
                "registry frame #1: pairing error 0x2101, correlation pairing 21212121; \
                 delivery: request waiter none, pairing inbox 0 of 0, plane 1 of 2; connection: 2"
            );

            // The control: room in the flow's own stream.
            let client = RegistryClient::offline(ClientConfig {
                plane_capacity: 1,
                ..ClientConfig::default()
            });
            let mut flow = client.subscribe_plane();
            let other = client.subscribe_plane(); // takes its copy, never read
            client
                .deliver_as_reader(1, vec![CtrlFrame::new(unavailable(pairing))])
                .await;
            let taken = wait_for_the_commit(&client, &mut flow, pairing, 300).await;
            drop(other);
            assert!(
                matches!(taken, Err(PairFlowError::Unavailable { code }) if code == PAIR_UNAVAILABLE),
                "now the flow takes the frame: {taken:?}"
            );
            assert_eq!(
                traced.the_one("registry frame #1: pairing error 0x2101, correlation pairing 21212121; delivery: request waiter none, pairing inbox 0 of 0, plane 2 of 2"),
                "registry frame #1: pairing error 0x2101, correlation pairing 21212121; \
                 delivery: request waiter none, pairing inbox 0 of 0, plane 2 of 2; connection: 1"
            );
            let taken_line = traced.the_one("pairing error frame taken");
            assert!(
                taken_line.ends_with("; flow: redeemer, waiting for: PairCommit, pairing 21212121"),
                "{taken_line}"
            );
        }

        /// Review 21: the task join stops after the commit on this round's
        /// error while it waits for `PairReady` — one closing line naming
        /// the stage, the failure and the round, the original error
        /// returned, and the journal left at `committed`.
        ///
        /// Goes red when no line, or more than one, closes the joiner's
        /// flow after the commit; when the stage, the round or the failure
        /// is not the one that stopped it; when the taken frame is not
        /// placed in the flow; or when the flow returns another error.
        #[tokio::test]
        async fn a_task_join_stops_waiting_for_ready_on_this_round_s_error() {
            let Some(traced) = diag_test::traced_process(TASK_READY_ERROR, Switch::On) else {
                return;
            };
            stops_waiting_for_ready(Entry::Task, &traced).await;
        }

        /// The same through the other joiner entry point, `redeem`: the
        /// daemon of the admission case joins that way, and its stop after
        /// the commit must close the same. Goes red with the first test's
        /// reasons, for `redeem` alone.
        #[tokio::test]
        async fn a_plain_redeem_stops_waiting_for_ready_on_this_round_s_error() {
            let Some(traced) = diag_test::traced_process(PLAIN_READY_ERROR, Switch::On) else {
                return;
            };
            stops_waiting_for_ready(Entry::Plain, &traced).await;
        }

        /// Review 21: the joiner's control connection is gone by the time
        /// it takes its commit, so `PairApplied` cannot be sent. Goes red
        /// when the stop is not placed at `send PairApplied`, when the
        /// line is missing or doubled, or when the journal moved past
        /// `committed` although the registry never heard this side.
        #[tokio::test]
        async fn a_joiner_stops_sending_pair_applied_without_a_connection() {
            let Some(traced) = diag_test::traced_process(APPLIED_OFFLINE, Switch::On) else {
                return;
            };
            let registry = registry::spawn().await;
            let parked = parked_at_the_commit(&registry, Entry::Task, test_config(), &traced).await;
            let Parked {
                owner_key,
                joiner,
                redeeming,
                joiner_dir,
                code,
                pairing_id,
                ..
            } = parked;
            let prefix = id_prefix(&pairing_id);

            // The plane outlives the connection, so the parked frame still
            // reaches the flow.
            joiner.client().simulate_connection_loss().await;
            assert_eq!(joiner.client().release_commits(), 1);
            let error = redeeming
                .await
                .expect("the joiner task")
                .expect_err("the joiner cannot confirm its apply");
            assert!(
                matches!(error, PairFlowError::Client(ClientError::Offline)),
                "the original error comes back, got `{}`",
                error_category(&error)
            );
            one_joiner_stop(
                &traced,
                "control connection is down",
                "send PairApplied",
                &prefix,
            );
            assert_eq!(traced.count("flow stopped with"), 1);

            let book = fs::read_to_string(joiner_dir.path().join(BOOK_NAME)).expect("the book");
            assert!(book.contains(&Fingerprint::from_public_key(&owner_key.public_key()).to_hex()));
            assert_eq!(
                phases(joiner_dir.path()),
                vec![(pairing_id, PairingPhase::Committed)]
            );
            nothing_private(&traced, &pairing_id, &code);
        }

        /// Review 21: the trust book cannot be written (a directory sits
        /// where the file goes). Goes red when the stop is not placed at
        /// `commit_peer`, when the journal moved on, or when the failure's
        /// own message — it names the book's directory — reaches the line.
        #[tokio::test]
        async fn a_joiner_stops_at_commit_peer_when_the_book_cannot_be_written() {
            let Some(traced) = diag_test::traced_process(BOOK_UNWRITABLE, Switch::On) else {
                return;
            };
            let registry = registry::spawn().await;
            let parked = parked_at_the_commit(&registry, Entry::Task, test_config(), &traced).await;
            let Parked {
                joiner,
                redeeming,
                joiner_dir,
                code,
                pairing_id,
                ..
            } = parked;
            let prefix = id_prefix(&pairing_id);

            fs::create_dir(joiner_dir.path().join(BOOK_NAME)).expect("block the trust book");
            assert_eq!(joiner.client().release_commits(), 1);
            let error = redeeming
                .await
                .expect("the joiner task")
                .expect_err("the trust book cannot be written");
            assert!(
                matches!(error, PairFlowError::Local(_)),
                "the failure is the local one, got `{}`",
                error_category(&error)
            );
            one_joiner_stop(&traced, "local pairing state error", "commit_peer", &prefix);
            assert_eq!(traced.count("flow stopped with"), 1);
            assert_eq!(
                phases(joiner_dir.path()),
                vec![(pairing_id, PairingPhase::Pending)],
                "the commit was never marked"
            );
            assert!(!joiner_dir.path().join(BOOK_NAME).is_file());
            nothing_private(&traced, &pairing_id, &code);
            let directory = joiner_dir.path().file_name().expect("the directory's name");
            assert!(
                traced
                    .lines()
                    .iter()
                    .all(|line| !line.contains(&*directory.to_string_lossy())),
                "no line names the identity directory"
            );
        }

        /// Review 21: the journal entry on disk already says `applied` — a
        /// stale replay, as when another run recovered this pairing in the
        /// meantime — so the commit cannot be marked; the mark fails closed
        /// with the pairing's full id in its message. Goes red when the
        /// stop is not placed at `journal.mark_committed`, when the flow
        /// rewrote the entry anyway, or when that id reaches the line.
        #[tokio::test]
        async fn a_joiner_stops_at_mark_committed_on_an_entry_already_applied() {
            let Some(traced) = diag_test::traced_process(STALE_ENTRY, Switch::On) else {
                return;
            };
            let registry = registry::spawn().await;
            let parked = parked_at_the_commit(&registry, Entry::Task, test_config(), &traced).await;
            let Parked {
                owner_key,
                joiner,
                redeeming,
                joiner_dir,
                code,
                pairing_id,
                ..
            } = parked;
            let prefix = id_prefix(&pairing_id);

            let entry = entry_of(joiner_dir.path(), &pairing_id);
            let pending = fs::read_to_string(&entry).expect("the joiner's pending entry");
            let stale = pending.replace("\nphase pending\n", "\nphase applied\n");
            assert_ne!(stale, pending, "the entry was pending");
            fs::write(&entry, &stale).expect("age the entry");

            assert_eq!(joiner.client().release_commits(), 1);
            let error = redeeming
                .await
                .expect("the joiner task")
                .expect_err("the stale entry refuses the mark");
            assert!(
                matches!(&error, PairFlowError::Io(error) if error.kind() == io::ErrorKind::InvalidData),
                "the journal's own refusal comes back"
            );
            one_joiner_stop(
                &traced,
                "local pairing store io error (InvalidData)",
                "journal.mark_committed",
                &prefix,
            );
            assert_eq!(traced.count("flow stopped with"), 1);

            // The book was written before the mark; the entry is as aged,
            // not marked by this flow.
            let book = fs::read_to_string(joiner_dir.path().join(BOOK_NAME)).expect("the book");
            assert!(book.contains(&Fingerprint::from_public_key(&owner_key.public_key()).to_hex()));
            assert_eq!(fs::read_to_string(&entry).expect("the entry"), stale);
            nothing_private(&traced, &pairing_id, &code);
        }

        /// Review 21: `PairReady` received and the local `applied` mark are
        /// two steps. The registry sends the real `PairReady` (the owner
        /// finished its half), and only then does the joiner's mark fail on
        /// a controlled local write failure. Goes red when the stop is not
        /// placed at `journal.mark_applied`, when the Ready-received line
        /// is missing or comes after the stop, when the journal shows
        /// `applied`, or when the line is missing or doubled.
        #[tokio::test]
        async fn a_joiner_stops_at_mark_applied_when_the_entry_cannot_be_replaced() {
            let Some(traced) = diag_test::traced_process(ENTRY_BLOCKED, Switch::On) else {
                return;
            };
            let registry = registry::spawn().await;
            let parked = parked_at_the_commit(&registry, Entry::Task, test_config(), &traced).await;
            let Parked {
                owner,
                owner_key,
                joiner,
                redeeming,
                answering,
                joiner_dir,
                code,
                pairing_id,
                ..
            } = parked;
            let prefix = id_prefix(&pairing_id);

            assert_eq!(joiner.client().release_commits(), 1);
            // The joiner books the owner, marks the commit, sends
            // `PairApplied`, and waits.
            wait_until("the joiner marks its commit", || {
                journal_has(joiner_dir.path(), pairing_id, PairingPhase::Committed)
            })
            .await;
            let entry = entry_of(joiner_dir.path(), &pairing_id);
            // The controlled local write failure. Elsewhere than Windows
            // the entry is removed under the flow instead, and the mark
            // refuses to invent it.
            #[cfg(windows)]
            let held = hold_entry_against_replacement(&entry);
            #[cfg(not(windows))]
            std::fs::remove_file(&entry).expect("remove the entry under the flow");

            // The owner finishes its half; the registry has both applies
            // and sends the real `PairReady`.
            assert_eq!(owner.client().release_commits(), 1);
            answering
                .await
                .expect("the owner task")
                .expect("the owner applies its side");
            let error = redeeming
                .await
                .expect("the joiner task")
                .expect_err("the joiner's local mark fails");
            match &error {
                PairFlowError::Io(error) => {
                    #[cfg(windows)]
                    assert_eq!(error.raw_os_error(), Some(32), "a sharing violation");
                    #[cfg(not(windows))]
                    assert_eq!(error.kind(), io::ErrorKind::NotFound);
                }
                other => panic!("the journal's own failure comes back, got {other:?}"),
            }

            // Ready was received — on record under the joiner's flow, before
            // the stop — and it is the local mark that failed.
            let ready_line = traced.the_one("; flow: redeemer, pairing ");
            assert!(
                ready_line.starts_with("applied sent and ready received, "),
                "{ready_line}"
            );
            assert!(traced.before(
                "applied sent and ready received",
                "redeemer: flow stopped with"
            ));
            #[cfg(windows)]
            let category = "local pairing store io error (os error 32)";
            #[cfg(not(windows))]
            let category = "local pairing store io error (NotFound)";
            one_joiner_stop(&traced, category, "journal.mark_applied", &prefix);
            assert_eq!(
                traced.count("flow stopped with"),
                1,
                "the owner's flow did not stop"
            );
            #[cfg(windows)]
            drop(held);

            let book = fs::read_to_string(joiner_dir.path().join(BOOK_NAME)).expect("the book");
            assert!(book.contains(&Fingerprint::from_public_key(&owner_key.public_key()).to_hex()));
            let phases = phases(joiner_dir.path());
            #[cfg(windows)]
            assert_eq!(
                phases,
                vec![(pairing_id, PairingPhase::Committed)],
                "the committed entry is still what the journal holds"
            );
            #[cfg(not(windows))]
            assert!(
                phases
                    .iter()
                    .all(|record| record.1 != PairingPhase::Applied),
                "nothing says applied: {phases:?}"
            );
            nothing_private(&traced, &pairing_id, &code);
        }

        /// The control: a pairing that completes writes no closing line on
        /// either side. Goes red when a successful flow closes with a
        /// stop line, or when `PairReady` received is not on record for
        /// each side under its own flow.
        #[tokio::test]
        async fn a_paired_flow_writes_no_stop_line() {
            let Some(traced) = diag_test::traced_process(SUCCESS, Switch::On) else {
                return;
            };
            let registry = registry::spawn().await;
            let parked = parked_at_the_commit(&registry, Entry::Task, test_config(), &traced).await;
            let Parked {
                owner,
                joiner,
                redeeming,
                answering,
                owner_dir,
                joiner_dir,
                code,
                pairing_id,
                ..
            } = parked;
            let prefix = id_prefix(&pairing_id);

            assert_eq!(joiner.client().release_commits(), 1);
            assert_eq!(owner.client().release_commits(), 1);
            let (owner_outcome, joiner_outcome) =
                tokio::time::timeout(LIMIT, async { tokio::join!(answering, redeeming) })
                    .await
                    .expect("both flows finish");
            owner_outcome
                .expect("the owner task")
                .expect("the owner applies its side");
            joiner_outcome
                .expect("the joiner task")
                .expect("the joiner applies its side");

            assert_eq!(
                traced.count("flow stopped with"),
                0,
                "{}",
                traced.lines().join("\n")
            );
            assert_eq!(
                traced.count("applied sent and ready received"),
                2,
                "one line per side"
            );
            assert!(
                traced
                    .the_one("; flow: redeemer, pairing ")
                    .ends_with(&format!("; flow: redeemer, pairing {prefix}")),
                "{}",
                traced.lines().join("\n")
            );
            assert!(traced
                .the_one("; flow: owner, pairing ")
                .ends_with(&format!("; flow: owner, pairing {prefix}")));
            for dir in [owner_dir.path(), joiner_dir.path()] {
                assert!(
                    phases(dir).contains(&(pairing_id, PairingPhase::Applied)),
                    "applied in {}",
                    dir.display()
                );
            }
            nothing_private(&traced, &pairing_id, &code);
        }

        /// The control for the half before the commit: its closing line is
        /// the one that already existed — no stage, written once — and the
        /// all-zero error frame keeps the round the flow waited for. Goes
        /// red when that line changes shape or count, or when an all-zero
        /// error is taken without saying which flow and round took it.
        #[tokio::test]
        async fn a_stop_before_the_commit_writes_one_line_without_a_stage() {
            let Some(traced) = diag_test::traced_process(BEFORE_COMMIT, Switch::On) else {
                return;
            };
            let registry = registry::spawn().await;
            let joiner_dir = tempfile::tempdir().unwrap();
            let joiner = orchestrator_on(
                &registry,
                "bob",
                &DeviceKeyPair::generate(),
                joiner_dir.path(),
                OrchestratorConfig {
                    redeem_window_ms: 1_500,
                    ..test_config()
                },
            )
            .await;
            // A parse-valid code nobody registered: the registry answers
            // the unified failure at the deadline, naming no round.
            let ghost = InviteCode::generate().to_display();
            let error = joiner.redeem(&ghost).await.expect_err("a ghost code fails");
            assert!(
                matches!(error, PairFlowError::Unavailable { code } if code == PAIR_UNAVAILABLE),
                "the original error comes back, got `{}`",
                error_category(&error)
            );

            // Which round the ghost was is on record with its acknowledgment.
            let acknowledged = traced.the_one("redeem acknowledged (pairing ");
            let prefix = acknowledged
                .split("redeem acknowledged (pairing ")
                .nth(1)
                .and_then(|rest| rest.get(..8))
                .expect("a pairing prefix in the line")
                .to_string();
            let taken = traced.the_one("pairing error frame taken");
            assert!(
                taken.starts_with(
                    "pairing error frame taken: code 0x2101, correlation all-zero, at "
                ),
                "{taken}"
            );
            assert!(
                taken.ends_with(&format!(
                    "; flow: redeemer, waiting for: PairChallenge, pairing {prefix}"
                )),
                "the all-zero error keeps the round waited for: {taken}"
            );
            closing_line(
                &traced.the_one("redeemer: flow stopped with"),
                "redeemer: flow stopped with `pairing target unavailable (0x2101)` ",
                " ms after it began, at ",
                "",
            );
            assert_eq!(traced.count("flow stopped with"), 1);
            assert_eq!(traced.count("after the commit"), 0);
            assert_eq!(phases(joiner_dir.path()), [], "nothing was journaled");
            assert!(!joiner_dir.path().join(BOOK_NAME).exists());
            nothing_private(&traced, &[0; 16], &ghost);
        }

        /// The control for the owner: its flow stops after the commit (its
        /// trust book cannot be written) and the joiner, never told Ready,
        /// gives up. One line per flow and none twice — the owner's finish
        /// is closed by its one outer line. Goes red when the owner's stop
        /// is written twice or not at all, when the joiner's stop is not
        /// the one of its own wait, or when the owner's failure message —
        /// it names the book's directory — reaches a line.
        #[tokio::test]
        async fn an_owner_stop_after_the_commit_writes_one_owner_line() {
            let Some(traced) = diag_test::traced_process(OWNER_STOP, Switch::On) else {
                return;
            };
            let registry = registry::spawn().await;
            let parked = parked_at_the_commit(
                &registry,
                Entry::Task,
                OrchestratorConfig {
                    ready_wait_ms: 1_500,
                    ..test_config()
                },
                &traced,
            )
            .await;
            let Parked {
                owner,
                joiner,
                redeeming,
                answering,
                owner_dir,
                joiner_dir,
                code,
                pairing_id,
                ..
            } = parked;
            let prefix = id_prefix(&pairing_id);

            fs::create_dir(owner_dir.path().join(BOOK_NAME)).expect("block the owner's book");
            assert_eq!(joiner.client().release_commits(), 1);
            assert_eq!(owner.client().release_commits(), 1);
            let (owner_outcome, joiner_outcome) =
                tokio::time::timeout(LIMIT, async { tokio::join!(answering, redeeming) })
                    .await
                    .expect("both flows end");
            let owner_error = owner_outcome
                .expect("the owner task")
                .expect_err("the owner's book cannot be written");
            assert!(
                matches!(owner_error, PairFlowError::Local(_)),
                "the owner's own error, got `{}`",
                error_category(&owner_error)
            );
            let joiner_error = joiner_outcome
                .expect("the joiner task")
                .expect_err("no Ready ever comes");
            assert!(
                matches!(joiner_error, PairFlowError::Timeout),
                "the joiner's own error, got `{}`",
                error_category(&joiner_error)
            );

            assert_eq!(
                traced.count("flow stopped with"),
                2,
                "one line per flow, none twice"
            );
            closing_line(
                &traced.the_one("owner: flow stopped with"),
                "owner: flow stopped with `local pairing state error` ",
                " ms after the offer, at ",
                "",
            );
            one_joiner_stop(
                &traced,
                "timed out waiting for the pairing flow",
                "wait PairReady",
                &prefix,
            );
            assert_eq!(
                phases(owner_dir.path()),
                vec![(pairing_id, PairingPhase::Pending)],
                "the owner never marked its commit"
            );
            assert_eq!(
                phases(joiner_dir.path()),
                vec![(pairing_id, PairingPhase::Committed)]
            );
            let directory = owner_dir.path().file_name().expect("the directory's name");
            assert!(
                traced
                    .lines()
                    .iter()
                    .all(|line| !line.contains(&*directory.to_string_lossy())),
                "no line names the identity directory"
            );
            nothing_private(&traced, &pairing_id, &code);
        }

        /// Review 21: a redeem refused while it still waits for its answer
        /// never reaches a flow's `wait_frame`. The evidence is written
        /// where its oneshot is taken, right after the dispatch line says
        /// the frame went to the request waiter, and right before the one
        /// closing line. Goes red when the answer is not named at the take,
        /// when the dispatch line does not tell the waiter apart from the
        /// inboxes, or when the order of the three lines is another.
        #[tokio::test]
        async fn a_redeem_refused_at_its_answer_is_taken_on_the_oneshot() {
            let Some(traced) = diag_test::traced_process(ONESHOT, Switch::On) else {
                return;
            };
            let registry = registry::spawn().await;
            let endpoint = |port: u16| {
                ak_signal::protocol_v2::EndpointBlob(
                    format!(r#"{{"addrs":["127.0.0.1:{port}"]}}"#).into_bytes(),
                )
            };
            let owner_dir = tempfile::tempdir().unwrap();
            let owner = orchestrator_on(
                &registry,
                "alice",
                &DeviceKeyPair::generate(),
                owner_dir.path(),
                test_config(),
            )
            .await;
            owner.spawn_owner_loop();
            let invite = owner
                .create_task_invite([0x61; 16], 0, endpoint(7200))
                .await
                .expect("claim the one-shot task code");

            let first_dir = tempfile::tempdir().unwrap();
            let first = orchestrator_on(
                &registry,
                "bob",
                &DeviceKeyPair::generate(),
                first_dir.path(),
                test_config(),
            )
            .await;
            first
                .redeem_task(&invite.display, endpoint(7201))
                .await
                .expect("the single join pairs");

            // The code's one redeem is spent: the registry refuses the
            // second one in the answer to `InviteRedeem` itself.
            let second_dir = tempfile::tempdir().unwrap();
            let second = orchestrator_on(
                &registry,
                "carol",
                &DeviceKeyPair::generate(),
                second_dir.path(),
                test_config(),
            )
            .await;
            let refused = second
                .redeem_task(&invite.display, endpoint(7202))
                .await
                .expect_err("the second join is refused");
            assert!(
                matches!(refused, PairFlowError::Rejected { code } | PairFlowError::Unavailable { code } if code == REDEEM_EXHAUSTED),
                "the original error comes back, got `{}`",
                error_category(&refused)
            );

            let dispatched = "registry frame #1: code-scoped error 0x2109; delivery: request waiter queued, pairing inbox 0 of 0, plane 1 of 1; connection: 1";
            assert_eq!(
                traced.the_one("code-scoped error 0x2109; delivery: "),
                dispatched
            );
            assert_eq!(
                traced.the_one("request answer taken: "),
                "request answer taken: InviteRedeem answered with code-scoped error 0x2109"
            );
            closing_line(
                &traced.the_one("redeemer: flow stopped with"),
                "redeemer: flow stopped with `pairing rejected: 0x2109` ",
                " ms after it began, at ",
                "",
            );
            assert_eq!(
                traced.count("flow stopped with"),
                1,
                "the first join and the owner paired"
            );
            assert!(traced.before(dispatched, "request answer taken: "));
            assert!(traced.before("request answer taken: ", "redeemer: flow stopped with"));
            assert_eq!(phases(second_dir.path()), [], "nothing was journaled");
            nothing_private(&traced, &[0; 16], &invite.display);
        }
    }
}
