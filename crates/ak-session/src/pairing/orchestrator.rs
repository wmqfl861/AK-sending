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
use std::time::Duration;

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
}

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
        })
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
                .invite_claim_with_id(request_id, code.lookup_digest(), ttl_secs)
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
        let committed = self.redeem_to_commit_on(code_str, &mut plane).await?;
        self.finish_committed(committed, &mut plane).await
    }

    /// [`PairingOrchestrator::redeem`] stopping right after the verified
    /// `PairCommit` — the checkpoint a crashed redeemer resumes from.
    pub async fn redeem_to_commit(
        &self,
        code_str: &str,
    ) -> Result<CommittedPairing, PairFlowError> {
        let mut plane = self.client.subscribe_plane();
        self.redeem_to_commit_on(code_str, &mut plane).await
    }

    /// Finish a committed pairing: trust-book apply, journal marks, the
    /// `PairApplied` confirmation, and the wait for `PairReady`.
    pub async fn finish_committed(
        &self,
        committed: CommittedPairing,
        plane: &mut mpsc::Receiver<CtrlMessage>,
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
        self.commit_peer(&pairing_id, peer_pub)?;
        self.journal.mark_committed(&pairing_id)?;
        let signature = self.key.sign(&hello2::applied_message(
            &pairing_id,
            &digest,
            committed.commit_revision,
        ));
        self.client
            .send_pair_applied(pairing_id, digest, committed.commit_revision, signature)
            .await
            .map_err(map_client)?;
        wait_ready(plane, pairing_id, digest, self.config.ready_wait_ms).await?;
        self.journal.mark_applied(&pairing_id)?;
        Ok(committed.verified)
    }

    async fn redeem_to_commit_on(
        &self,
        code_str: &str,
        plane: &mut mpsc::Receiver<CtrlMessage>,
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
        let ack = self
            .client
            .invite_redeem(ak_signal::protocol_v2::RedeemRequest {
                request_id,
                lookup_digest: code.lookup_digest(),
                redeemer_nonce: nonce,
                deadline_ms: deadline,
                request_mac,
                signature,
            })
            .await
            .map_err(map_client)?;
        let pairing_id = ack.pairing_id;

        // ← PairChallenge (relayed owner transcript + owner proof).
        let (transcript, owner_proof) =
            match wait_frame(plane, pairing_id, deadline.saturating_add(5_000), |frame| {
                pairing_challenge_of(frame, pairing_id)
            })
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

        // ← PairCommit (authoritative).
        let commit = match wait_frame(
            plane,
            pairing_id,
            deadline.saturating_add(10_000),
            |frame| pair_commit_of(frame, pairing_id),
        )
        .await?
        {
            Some(commit) => commit,
            None => return Err(PairFlowError::Timeout),
        };
        let verified = verify_commit_full(&commit, &auth_key, now_unix_ms())?;
        Ok(CommittedPairing {
            pairing_id,
            peer_fingerprint: Fingerprint::from_public_key(&transcript.owner_device_id),
            commit_revision: commit.commit_revision,
            verified,
        })
    }

    // ---- owner path ----------------------------------------------------------

    /// Spawn the resident owner loop: consumes the pairing plane, answers
    /// every `InviteRedeemOffered` for a code this device holds. Duplicate
    /// offers (Hello catch-up re-pushes) are deduplicated per pairing id.
    pub fn spawn_owner_loop(&self) -> JoinHandle<()> {
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
                    let orchestrator = orchestrator.clone();
                    tokio::spawn(async move {
                        if let Err(err) = orchestrator
                            .owner_answer_offer(
                                code_id,
                                code_revision,
                                pairing_id,
                                redeemer_device_id,
                                redeem,
                            )
                            .await
                        {
                            log::warn!("owner pairing flow failed ({}): {err}", hex(&pairing_id));
                            let _ = orchestrator
                                .client
                                .pair_abort(pairing_id, AbortReason::Protocol)
                                .await;
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
        let mut plane = self.client.subscribe_plane();
        let (auth_key, secret_ref, lookup_digest) = {
            let secrets = self.secrets.lock().expect("orchestrator lock");
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

        // ← PairProof (redeemer's half).
        let redeemer_proof = match wait_frame(
            &mut plane,
            pairing_id,
            redeem.deadline_ms.saturating_add(5_000),
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

        // → PairPrepared, then ← PairCommit.
        let prepared = self
            .key
            .sign(&hello2::prepared_message(&pairing_id, &digest));
        self.client
            .send_pair_prepared(pairing_id, digest, prepared)
            .await
            .map_err(map_client)?;
        let commit = match wait_frame(
            &mut plane,
            pairing_id,
            redeem.deadline_ms.saturating_add(10_000),
            |frame| pair_commit_of(frame, pairing_id),
        )
        .await?
        {
            Some(commit) => commit,
            None => return Err(PairFlowError::Timeout),
        };
        let verified = verify_commit_full(&commit, &auth_key, now_unix_ms())?;
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
    /// [`PairingOrchestrator::remember_authenticated_transcript`])
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
                    if wait_ready(&mut plane, pairing_id, [0u8; 32], 10_000)
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
                        &mut plane,
                        pairing_id,
                        now_unix_ms().saturating_add(10_000),
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
                            // transcript's own deadline (the commit happened
                            // before it, which the server enforced).
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
                    if wait_ready(&mut plane, pairing_id, digest, self.config.ready_wait_ms)
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

/// Wait until `extract` returns `Some` for a frame of `pairing_id`'s flow.
/// `Ok(None)` = timeout. Pairing-scoped `PairError` frames (deadline
/// ghosts, revoke/sweep aborts — including the all-zero object id the
/// ghost path uses) surface as unified flow errors.
async fn wait_frame<T, F>(
    plane: &mut mpsc::Receiver<CtrlMessage>,
    pairing_id: [u8; 16],
    until_ms: u64,
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
            scope: ak_signal::protocol_v2::ErrorScope::Pairing,
            object_id,
            code,
            ..
        } = &frame
        {
            if *object_id == pairing_id || *object_id == [0u8; 16] {
                return Err(PairFlowError::from_pair_code(*code));
            }
        }
        if let Some(value) = extract(&frame) {
            return Ok(Some(value));
        }
    }
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
    plane: &mut mpsc::Receiver<CtrlMessage>,
    pairing_id: [u8; 16],
    digest: [u8; 32],
    wait_ms: u64,
) -> Result<(), PairFlowError> {
    let until = now_unix_ms().saturating_add(wait_ms);
    match wait_frame(plane, pairing_id, until, |frame| match frame {
        CtrlMessage::PairReady {
            pairing_id: pid,
            transcript_digest,
            ..
        } if *pid == pairing_id && (digest == [0u8; 32] || *transcript_digest == digest) => {
            Some(())
        }
        _ => None,
    })
    .await?
    {
        Some(()) => Ok(()),
        None => Err(PairFlowError::Timeout),
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
}
