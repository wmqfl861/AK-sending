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
//! full MAC verification; without the secret the recovery verifies the
//! transcript digest and both Ed25519 signatures only (degraded,
//! documented) — the MACs were verified before the crash and the commit
//! is authoritative.
//!
//! ## Abort policy
//!
//! A failed owner flow sends `PairAbort(Protocol)` so the server tombstones
//! the transaction; redeem failures surface the server's unified
//! `PAIR_UNAVAILABLE` unchanged.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Ok(PairingOrchestrator {
            journal: Arc::new(PairingJournal::open(&id_dir.join(JOURNAL_SUBDIR))?),
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
    /// `lookup_digest` restores full MAC verification; without it the
    /// recovery verifies the transcript digest and both device signatures
    /// only — the MACs were verified before the crash and the server's
    /// commit is authoritative. Returns the pairing ids brought to
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
                        None => verify_commit_degraded(&commit)?,
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

/// Degraded recovery verification without the invite secret: transcript
/// digest integrity plus both Ed25519 signatures under the keys the
/// transcript itself names (the MACs were verified pre-crash; the server's
/// commit is authoritative).
fn verify_commit_degraded(commit: &CommitView) -> Result<(), PairFlowError> {
    let transcript = PairTranscript::from_bytes(&commit.transcript);
    let mut digest_bytes = [0u8; 32];
    digest_bytes.copy_from_slice(blake3::hash(&commit.transcript).as_bytes());
    if transcript.transcript_digest() != digest_bytes {
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
