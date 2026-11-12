//! The wave-21 real [`PairingTransport`] adapter over
//! [`RegistryClient`].
//!
//! This is the "真实适配器挂入点" of the wave-20 runtime: construct it over
//! the device's one [`RegistryClient`] and pass `Arc<RealTransport>` as
//! [`RuntimeConfig::transport`](crate::runtime::RuntimeConfig::transport)
//! — the injection seam the Fake-DI runtime left open. The resident
//! listener then consumes **real** pairing events
//! ([`PairingEvent::RedeemOffered`] from real `InviteRedeemOffered` pushes,
//! [`PairingEvent::PairReady`] from real `PairCommit`/`PairReady` frames)
//! and submits inbound receive tasks exactly as it did against the fake.
//!
//! Method mapping:
//!
//! | trait method | real exchange |
//! |---|---|
//! | `claim_invite` | `InviteClaim` → `InviteClaimed` |
//! | `redeem` | `InviteRedeem` → `InviteRedeemPending` |
//! | `confirm_pairing` | `PairProof` + `PairPrepared`, then awaits `PairCommit` |
//! | `open_session` | `ConnectRequest` → `SessionOpened` toward the peer learned from the commit |
//! | `next_pairing_event` | merged pairing-plane stream + connection state |
//! | `is_connected` | the live TLS connection state |
//!
//! Known divergence (documented, by design): the trait's
//! `confirm_pairing` promises a `PairingReadyInfo`, but reaching the wire
//! `PairReady` requires the **local trust-book apply** (`PairApplied`),
//! which is the orchestrator's transactional job
//! ([`crate::pairing::PairingOrchestrator`]), not a transport call. The
//! adapter therefore drives the transcript to the authoritative server
//! commit and reports the commit facts (revision, code, peer); runtime
//! tasks that need a fully applied relation should be submitted with
//! `PairingPlan::Ready` after the orchestrator finished.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ak_identity::pairing_txn::random_id16;
use ak_signal::protocol_v2::{CtrlMessage, PairProof as WireProof};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::pairing::client::{ClientState, RegistryClient};
use crate::pairing::error::ClientError;
use crate::runtime::transport::{
    InviteClaimRequest, InviteClaimed, OpenSessionInfo, PairingConfirmation, PairingError,
    PairingEvent, PairingFuture, PairingReadyInfo, PairingTransport, RedeemAccepted, RedeemPayload,
};
use crate::runtime::transport::{PairingId as RtPairingId, PeerFingerprint as RtPeerFingerprint};
use crate::scheduler::key::{CodeId as RtCodeId, RegistryId as RtRegistryId};

/// How long `confirm_pairing` waits for the authoritative `PairCommit`.
const COMMIT_WAIT: Duration = Duration::from_secs(30);
/// Bound of the summarized event queue the pump feeds.
const EVENT_QUEUE_CAPACITY: usize = 256;

/// Facts learned from a `PairCommit` (the only frame that names the code
/// and both devices of a finished transcript).
#[derive(Debug, Clone, Copy)]
struct CommitInfo {
    code_id: [u8; 16],
    peer_device_id: [u8; 32],
}

/// Real [`PairingTransport`] over one shared [`RegistryClient`].
///
/// Clone-cheap handle; the summarizer pump is spawned once per underlying
/// client and stops when the client is closed.
#[derive(Clone)]
pub struct RealTransport {
    client: RegistryClient,
    events: Arc<AsyncMutex<mpsc::Receiver<PairingEvent>>>,
    commits: Arc<StdMutex<HashMap<[u8; 16], CommitInfo>>>,
}

impl RealTransport {
    /// Build the adapter and spawn the summarizer pump.
    pub fn new(client: RegistryClient) -> Self {
        let (tx, rx) = mpsc::channel::<PairingEvent>(EVENT_QUEUE_CAPACITY);
        let commits = Arc::new(StdMutex::new(HashMap::new()));
        let pump_client = client.clone();
        let pump_commits = Arc::clone(&commits);
        let device_id = pump_client.device_id();
        let registry_id = RtRegistryId::from_bytes(pump_client.registry_id());
        // Subscribe synchronously: pairing frames pushed between `new` and
        // the pump's first poll would otherwise be dropped.
        let plane = pump_client.subscribe_plane();
        tokio::spawn(async move {
            summarizer_pump(pump_client, plane, pump_commits, device_id, registry_id, tx).await;
        });
        RealTransport {
            client,
            events: Arc::new(AsyncMutex::new(rx)),
            commits,
        }
    }

    /// The underlying client (the orchestrator shares it).
    pub fn client(&self) -> &RegistryClient {
        &self.client
    }

    fn map_client_error(err: ClientError) -> PairingError {
        match err {
            ClientError::Offline | ClientError::ConnectionLost => PairingError::Offline,
            ClientError::Closed => PairingError::Offline,
            ClientError::Timeout { .. } => PairingError::Offline,
            other => PairingError::rejected(&other.to_string()),
        }
    }
}

/// Translate the pairing plane plus connection state into the runtime's
/// summarized [`PairingEvent`]s.
async fn summarizer_pump(
    client: RegistryClient,
    mut plane: mpsc::Receiver<CtrlMessage>,
    commits: Arc<StdMutex<HashMap<[u8; 16], CommitInfo>>>,
    device_id: [u8; 32],
    registry_id: RtRegistryId,
    tx: mpsc::Sender<PairingEvent>,
) {
    let mut state = client.state_watcher();
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    return;
                }
                let disconnected = matches!(*state.borrow(), ClientState::Disconnected);
                if disconnected {
                    let _ = tx.send(PairingEvent::Disconnected).await;
                }
            }
            frame = plane.recv() => {
                let Some(frame) = frame else { return };
                let event = summarize(&frame, &commits, device_id, registry_id);
                let event = match event {
                    Some(event) => event,
                    None => continue,
                };
                let _ = tx.send(event).await;
            }
        }
    }
}

/// One frame → one summarized event (`None` = not runtime-relevant).
fn summarize(
    frame: &CtrlMessage,
    commits: &StdMutex<HashMap<[u8; 16], CommitInfo>>,
    device_id: [u8; 32],
    registry_id: RtRegistryId,
) -> Option<PairingEvent> {
    match frame {
        CtrlMessage::InviteRedeemOffered {
            code_id,
            pairing_id,
            redeemer_device_id,
            ..
        } => Some(PairingEvent::RedeemOffered {
            code_id: RtCodeId::from_bytes(*code_id),
            pairing_id: RtPairingId::from_bytes(*pairing_id),
            peer: peer_fingerprint(*redeemer_device_id),
        }),
        CtrlMessage::PairCommit { transcript, .. } => {
            let typed = ak_identity::pairing_txn::PairTranscript::from_bytes(transcript);
            let peer = if typed.owner_device_id == device_id {
                typed.redeemer_device_id
            } else {
                typed.owner_device_id
            };
            commits.lock().expect("real transport commits lock").insert(
                typed.pairing_id,
                CommitInfo {
                    code_id: typed.code_id,
                    peer_device_id: peer,
                },
            );
            Some(PairingEvent::PairReady {
                registry_id,
                code_id: RtCodeId::from_bytes(typed.code_id),
                pairing_id: RtPairingId::from_bytes(typed.pairing_id),
                peer: peer_fingerprint(peer),
            })
        }
        CtrlMessage::PairReady { pairing_id, .. } => {
            // The Ready frame carries no code/peer; use what the commit
            // taught us. Without a commit (fresh process) the listener has
            // no group key to submit under, so the frame is skipped.
            commits
                .lock()
                .expect("real transport commits lock")
                .get(pairing_id)
                .map(|info| PairingEvent::PairReady {
                    registry_id,
                    code_id: RtCodeId::from_bytes(info.code_id),
                    pairing_id: RtPairingId::from_bytes(*pairing_id),
                    peer: peer_fingerprint(info.peer_device_id),
                })
        }
        CtrlMessage::PairError {
            scope,
            object_id,
            code,
            ..
        } if *scope == ak_signal::protocol_v2::ErrorScope::Pairing => {
            Some(PairingEvent::PairAborted {
                pairing_id: RtPairingId::from_bytes(*object_id),
                reason: *code as u8,
            })
        }
        _ => None,
    }
}

/// BLAKE3 fingerprint of a device public key, in the runtime's type.
fn peer_fingerprint(device_id: [u8; 32]) -> RtPeerFingerprint {
    let fp = ak_identity::Fingerprint::from_public_key(&device_id);
    RtPeerFingerprint::from_bytes(*fp.as_bytes())
}

fn map_claim(claimed: ak_signal::client_v2::ClaimedInvite) -> InviteClaimed {
    InviteClaimed {
        code_id: RtCodeId::from_bytes(claimed.code_id),
        created_ms: claimed.created_ms,
        expires_ms: claimed.expires_ms,
        revision: claimed.revision,
    }
}

impl PairingTransport for RealTransport {
    fn claim_invite(&self, claim: InviteClaimRequest) -> PairingFuture<'_, InviteClaimed> {
        let client = self.client.clone();
        Box::pin(async move {
            let claimed = client
                .invite_claim(claim.lookup_digest, claim.ttl_secs)
                .await
                .map_err(Self::map_client_error)?;
            Ok(map_claim(claimed))
        })
    }

    fn redeem(&self, request: RedeemPayload) -> PairingFuture<'_, RedeemAccepted> {
        let client = self.client.clone();
        Box::pin(async move {
            let ack = client
                .invite_redeem(ak_signal::protocol_v2::RedeemRequest {
                    request_id: request.request_id,
                    lookup_digest: request.lookup_digest,
                    redeemer_nonce: request.redeemer_nonce,
                    deadline_ms: request.deadline_ms,
                    request_mac: request.request_mac,
                    signature: request.signature,
                })
                .await
                .map_err(Self::map_client_error)?;
            Ok(RedeemAccepted {
                pairing_id: RtPairingId::from_bytes(ack.pairing_id),
                deadline_ms: ack.deadline_ms,
            })
        })
    }

    fn confirm_pairing(
        &self,
        confirmation: PairingConfirmation,
    ) -> PairingFuture<'_, PairingReadyInfo> {
        let client = self.client.clone();
        let commits = Arc::clone(&self.commits);
        Box::pin(async move {
            let pairing_id = *confirmation.pairing_id.as_bytes();
            let digest = confirmation.transcript_digest;
            let (mac, sig) = confirmation.proof.split_at(32);
            let mut mac_arr = [0u8; 32];
            mac_arr.copy_from_slice(mac);
            let mut sig_arr = [0u8; 64];
            sig_arr.copy_from_slice(sig);
            let proof = WireProof {
                mac: mac_arr,
                signature: sig_arr,
            };
            client
                .send_pair_proof(pairing_id, digest, proof)
                .await
                .map_err(Self::map_client_error)?;
            let prepared_signature = client_sign_prepared(&client, &pairing_id, &digest)?;
            client
                .send_pair_prepared(pairing_id, digest, prepared_signature)
                .await
                .map_err(Self::map_client_error)?;

            // Await the authoritative commit on this pairing's stream.
            let mut inbox = client.subscribe_pairing(pairing_id);
            let deadline = tokio::time::Instant::now() + COMMIT_WAIT;
            loop {
                let frame = tokio::time::timeout_at(deadline, inbox.recv())
                    .await
                    .map_err(|_| PairingError::Expired)?
                    .ok_or(PairingError::Offline)?;
                match frame {
                    CtrlMessage::PairCommit {
                        transcript,
                        commit_revision,
                        ..
                    } => {
                        let typed =
                            ak_identity::pairing_txn::PairTranscript::from_bytes(&transcript);
                        let peer = if typed.owner_device_id == client.device_id() {
                            typed.redeemer_device_id
                        } else {
                            typed.owner_device_id
                        };
                        commits.lock().expect("real transport commits lock").insert(
                            pairing_id,
                            CommitInfo {
                                code_id: typed.code_id,
                                peer_device_id: peer,
                            },
                        );
                        return Ok(PairingReadyInfo {
                            pairing_id: RtPairingId::from_bytes(pairing_id),
                            code_id: RtCodeId::from_bytes(typed.code_id),
                            peer: peer_fingerprint(peer),
                            commit_revision,
                        });
                    }
                    CtrlMessage::PairError { code, .. } => {
                        return Err(PairingError::rejected(&format!("pair error 0x{code:04x}")));
                    }
                    _ => continue,
                }
            }
        })
    }

    fn open_session(
        &self,
        pairing_id: RtPairingId,
        code_id: RtCodeId,
    ) -> PairingFuture<'_, OpenSessionInfo> {
        let client = self.client.clone();
        let commits = Arc::clone(&self.commits);
        Box::pin(async move {
            let pid = *pairing_id.as_bytes();
            let info = commits
                .lock()
                .expect("real transport commits lock")
                .get(&pid)
                .copied();
            let Some(info) = info else {
                return Err(PairingError::rejected(
                    "no committed transcript for pairing",
                ));
            };
            let connection_id = random_id16();
            let opened = client
                .connect_peer(info.peer_device_id, pid, *code_id.as_bytes(), connection_id)
                .await
                .map_err(Self::map_client_error)?;
            Ok(OpenSessionInfo {
                session_id: opened.session,
                expires_ms: opened.expires_ms,
            })
        })
    }

    fn next_pairing_event(&self) -> PairingFuture<'_, PairingEvent> {
        let events = self.events.clone();
        Box::pin(async move {
            let mut rx = events.lock().await;
            rx.recv().await.ok_or(PairingError::Offline)
        })
    }

    fn is_connected(&self) -> bool {
        self.client.is_connected()
    }
}

/// Sign the frozen `PairPrepared` domain with the client's device key.
fn client_sign_prepared(
    client: &RegistryClient,
    pairing_id: &[u8; 16],
    digest: &[u8; 32],
) -> Result<[u8; 64], PairingError> {
    let message = ak_signal::server_v2::hello2::prepared_message(pairing_id, digest);
    Ok(client.sign_with_device_key(&message))
}
