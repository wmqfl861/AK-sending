//! Single-reader dispatch: one task consumes every inbound v2 frame and
//! routes it to the bounded inbox of the matching waiter or subscriber.
//!
//! This is the fix for the v1 defect named in the pairing plan: v1 pushed
//! every decoded message onto **one shared unbounded** channel
//! (`ak-signal` issue tracked in `docs/plans/pairing-code-plan-part2.md`,
//! contract `docs/contracts/pairing-flow.md` §11 "客户端请求 map / 收件箱
//! 有界"). v2 instead keeps:
//!
//! * a bounded inbox (default 64) per request waiter / event subscriber,
//! * a bounded registry (waiters are removed on completion, timeout or
//!   cancellation),
//! * and a drop counter + debug log for frames with no matching key, so a
//!   misbehaving server can neither wedge the reader nor balloon memory.
//!
//! The reader never `await`s on delivery: `try_send` with count-and-drop on
//! a full or closed inbox, then it moves on to the next frame.

use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::protocol_v2::{CtrlMessage, ErrorScope, PairingId, RequestId, SessionId, Transcript256};

/// Byte offset of `pairing_id` inside the frozen 256-byte `PairTranscript`
/// (protocol-v2.md §5: `registry_id[32] code_id[16] code_revision:u64
/// pairing_id[16] ...` → offset 56). Routing-only knowledge: the dispatcher
/// never validates transcript contents, that is ak-identity's typed layer.
const TRANSCRIPT_PAIRING_OFFSET: usize = 32 + 16 + 8;

/// Which keyed inbox an inbound message belongs to. `Request`, `Ping` and
/// `Connection` are one-shot (a single response resolves the waiter);
/// `Pairing`, `Session` are durable subscriptions that survive reconnects.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum DispatchKey {
    /// Response to an idempotent request (`InviteClaimed`, `PairStatusReply`,
    /// `PairError`, ...).
    Request(RequestId),
    /// `Pong` matching a `Ping` nonce.
    Ping([u8; 16]),
    /// `SessionOpened` answering our `ConnectRequest` (keyed by the
    /// connection id the caller chose).
    Connection([u8; 16]),
    /// Push stream for one pairing transaction (offer, proofs, commit,
    /// ready). Durable across reconnects.
    Pairing(PairingId),
    /// Relay stream inside one open coordination session. Durable.
    Session(SessionId),
}

impl DispatchKey {
    /// True for one-shot waiter keys that a single response resolves.
    fn is_transient(&self) -> bool {
        matches!(
            self,
            DispatchKey::Request(_) | DispatchKey::Ping(_) | DispatchKey::Connection(_)
        )
    }

    /// Build a key from a `PairError.scope` + `object_id` pair.
    fn from_scope(scope: ErrorScope, object_id: [u8; 16]) -> Option<DispatchKey> {
        match scope {
            ErrorScope::Request => Some(DispatchKey::Request(object_id)),
            ErrorScope::Pairing => Some(DispatchKey::Pairing(object_id)),
            ErrorScope::Connection => Some(DispatchKey::Connection(object_id)),
            ErrorScope::Session => Some(DispatchKey::Session(object_id)),
            ErrorScope::None | ErrorScope::Code => None,
        }
    }
}

/// Routing keys for one inbound message, in priority order (first match
/// wins). Empty means: general event inbox (`PairChallenge` from the server,
/// `ServerError`, stray pushes) — those have no keyable id on the envelope.
fn routing_keys(message: &CtrlMessage) -> Vec<DispatchKey> {
    match message {
        CtrlMessage::InviteClaimed { request_id, .. }
        | CtrlMessage::InviteListPage { request_id, .. }
        | CtrlMessage::InviteRevoked { request_id, .. }
        | CtrlMessage::InviteRedeemPending { request_id, .. }
        | CtrlMessage::PairStatusReply { request_id, .. } => {
            vec![DispatchKey::Request(*request_id)]
        }
        CtrlMessage::PairError {
            request_id,
            scope,
            object_id,
            ..
        } => {
            let mut keys = vec![DispatchKey::Request(*request_id)];
            if let Some(key) = DispatchKey::from_scope(*scope, *object_id) {
                keys.push(key);
            }
            keys
        }
        CtrlMessage::Pong { nonce } => vec![DispatchKey::Ping(*nonce)],
        CtrlMessage::SessionOpened { connection_id, .. } => {
            vec![DispatchKey::Connection(*connection_id)]
        }
        CtrlMessage::Signal { session, .. } => vec![DispatchKey::Session(*session)],
        CtrlMessage::InviteRedeemOffered { pairing_id, .. }
        | CtrlMessage::PairProof { pairing_id, .. }
        | CtrlMessage::PairReady { pairing_id, .. } => vec![DispatchKey::Pairing(*pairing_id)],
        // PairCommit carries only the frozen transcript; the pairing id sits
        // at byte 56 (see TRANSCRIPT_PAIRING_OFFSET).
        CtrlMessage::PairCommit { transcript, .. } => {
            vec![DispatchKey::Pairing(transcript_pairing_id(transcript))]
        }
        // ServerError / PairChallenge(server→redeemer) / stray AuthChallenge
        // or Welcome: no envelope id to route by.
        CtrlMessage::ServerError { .. }
        | CtrlMessage::PairChallenge { .. }
        | CtrlMessage::AuthChallenge { .. }
        | CtrlMessage::Welcome { .. } => Vec::new(),
        // Messages that must never arrive from the server; the direction
        // layer already rejects them, this arm only keeps the match total.
        _ => Vec::new(),
    }
}

/// Extract the pairing id from a frozen 256-byte transcript (routing only).
fn transcript_pairing_id(transcript: &Transcript256) -> PairingId {
    let mut id = [0u8; 16];
    id.copy_from_slice(&transcript[TRANSCRIPT_PAIRING_OFFSET..TRANSCRIPT_PAIRING_OFFSET + 16]);
    id
}

/// What happened to one routed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteOutcome {
    /// Delivered to at least one inbox.
    Delivered,
    /// No matching waiter, subscriber or event listener.
    Unmatched,
}

/// The bounded dispatch registry. Guarded by a std mutex held only for
/// synchronous, non-await sections.
#[derive(Debug, Default)]
pub(crate) struct DispatchRegistry {
    slots: HashMap<DispatchKey, Vec<mpsc::Sender<CtrlMessage>>>,
    events: Vec<mpsc::Sender<CtrlMessage>>,
}

impl DispatchRegistry {
    pub(crate) fn new() -> Self {
        DispatchRegistry::default()
    }

    /// Register a one-shot waiter. Returns the sender (for cancellation
    /// bookkeeping) and the receiver. Fails if a waiter already exists under
    /// a transient key (duplicate request in flight).
    pub(crate) fn register_waiter(
        &mut self,
        key: DispatchKey,
        capacity: usize,
    ) -> Result<(mpsc::Sender<CtrlMessage>, mpsc::Receiver<CtrlMessage>), ()> {
        let entry = self.slots.entry(key).or_default();
        if !entry.is_empty() {
            return Err(());
        }
        let (tx, rx) = mpsc::channel(capacity.max(1));
        entry.push(tx.clone());
        Ok((tx, rx))
    }

    /// Subscribe to a pairing / session stream (multiple subscribers
    /// allowed; durable across reconnects).
    pub(crate) fn subscribe(
        &mut self,
        key: DispatchKey,
        capacity: usize,
    ) -> mpsc::Receiver<CtrlMessage> {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        self.slots.entry(key).or_default().push(tx);
        rx
    }

    /// Subscribe to the general event inbox (unkeyed server pushes).
    pub(crate) fn subscribe_events(&mut self, capacity: usize) -> mpsc::Receiver<CtrlMessage> {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        self.events.push(tx);
        rx
    }

    /// Remove the exact sender registered for `key` (waiter cancellation /
    /// scope exit). Comparing senders keeps a re-registered retry from
    /// removing its own replacement.
    pub(crate) fn remove_if_match(
        &mut self,
        key: &DispatchKey,
        sender: &mpsc::Sender<CtrlMessage>,
    ) {
        if let Some(entry) = self.slots.get_mut(key) {
            entry.retain(|tx| !tx.same_channel(sender));
            if entry.is_empty() {
                self.slots.remove(key);
            }
        }
    }

    /// Drop every one-shot waiter (request / ping / connection). Their
    /// receivers observe the channel close and report `ConnectionLost`.
    /// Pairing, session and event subscriptions survive — committed
    /// transactions recover across reconnects (pairing-flow §6).
    pub(crate) fn clear_transients(&mut self) {
        self.slots.retain(|key, entry| {
            if key.is_transient() {
                entry.clear();
                false
            } else {
                true
            }
        });
    }

    /// Drop everything (client closed).
    pub(crate) fn clear_all(&mut self) {
        self.slots.clear();
        self.events.clear();
    }

    /// Deliver one inbound message. Never blocks: full inboxes count a drop,
    /// closed inboxes are pruned; returns [`RouteOutcome::Unmatched`] when
    /// nothing matched so the caller can bump its counter and log.
    pub(crate) fn route(&mut self, message: CtrlMessage) -> RouteOutcome {
        let keys = routing_keys(&message);
        let mut delivered = false;
        let mut stale: Vec<DispatchKey> = Vec::new();

        for key in keys {
            if let Some(entry) = self.slots.get_mut(&key) {
                for tx in entry.iter() {
                    match tx.try_send(message.clone()) {
                        Ok(()) => delivered = true,
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            log::warn!("client_v2: inbox for {key:?} full, frame dropped");
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {}
                    }
                }
                entry.retain(|tx| !tx.is_closed());
                if entry.is_empty() {
                    stale.push(key);
                }
            }
        }
        for key in stale {
            self.slots.remove(&key);
        }

        if delivered {
            return RouteOutcome::Delivered;
        }

        // No keyed inbox matched. Only unkeyed server pushes (ServerError,
        // server→redeemer PairChallenge, stray AuthChallenge/Welcome) reach
        // the general event inbox; a *keyed* frame with no waiter is a stale
        // or unsolicited response and is counted as unmatched, never dumped
        // into the event stream.
        let unkeyed_push = matches!(
            message,
            CtrlMessage::ServerError { .. }
                | CtrlMessage::PairChallenge { .. }
                | CtrlMessage::AuthChallenge { .. }
                | CtrlMessage::Welcome { .. }
        );
        if !unkeyed_push {
            return RouteOutcome::Unmatched;
        }
        let mut delivered_event = false;
        for tx in self.events.iter() {
            match tx.try_send(message.clone()) {
                Ok(()) => delivered_event = true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    log::warn!("client_v2: event inbox full, frame dropped");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            }
        }
        self.events.retain(|tx| !tx.is_closed());
        if delivered_event {
            RouteOutcome::Delivered
        } else {
            RouteOutcome::Unmatched
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claimed(id: u8) -> CtrlMessage {
        CtrlMessage::InviteClaimed {
            request_id: [id; 16],
            code_id: [id + 1; 16],
            created_ms: 1,
            expires_ms: 0,
            revision: 1,
        }
    }

    #[tokio::test]
    async fn routes_response_to_matching_waiter_only() {
        let mut reg = DispatchRegistry::new();
        let (_, mut rx1) = reg
            .register_waiter(DispatchKey::Request([1; 16]), 4)
            .unwrap();
        let _ = reg
            .register_waiter(DispatchKey::Request([2; 16]), 4)
            .unwrap();

        assert_eq!(reg.route(claimed(1)), RouteOutcome::Delivered);
        assert_eq!(reg.route(claimed(9)), RouteOutcome::Unmatched); // nobody waits for 9
        assert!(matches!(
            rx1.try_recv(),
            Ok(CtrlMessage::InviteClaimed { .. })
        ));
        assert!(rx1.try_recv().is_err(), "waiter inbox stays empty");
    }

    #[tokio::test]
    async fn duplicate_waiter_key_is_rejected() {
        let mut reg = DispatchRegistry::new();
        assert!(reg
            .register_waiter(DispatchKey::Connection([5; 16]), 4)
            .is_ok());
        assert!(reg
            .register_waiter(DispatchKey::Connection([5; 16]), 4)
            .is_err());
    }

    #[tokio::test]
    async fn clear_transients_fails_waiters_keeps_streams() {
        let mut reg = DispatchRegistry::new();
        let (_, mut req) = reg
            .register_waiter(DispatchKey::Request([1; 16]), 4)
            .unwrap();
        let mut pairing = reg.subscribe(DispatchKey::Pairing([2; 16]), 4);

        reg.clear_transients();
        assert!(req.try_recv().is_err(), "request waiter channel closed");
        assert!(
            reg.register_waiter(DispatchKey::Request([1; 16]), 4)
                .is_ok(),
            "transient key reusable after clear"
        );

        let push = CtrlMessage::PairReady {
            pairing_id: [2; 16],
            transcript_digest: [0; 32],
            commit_revision: 1,
            owner_applied_signature: [0; 64],
            redeemer_applied_signature: [0; 64],
        };
        assert_eq!(reg.route(push.clone()), RouteOutcome::Delivered);
        assert!(matches!(
            pairing.try_recv(),
            Ok(CtrlMessage::PairReady { .. })
        ));
    }

    #[test]
    fn pair_commit_routes_by_transcript_embedded_pairing_id() {
        let mut transcript = [0u8; 256];
        transcript[TRANSCRIPT_PAIRING_OFFSET..TRANSCRIPT_PAIRING_OFFSET + 16]
            .copy_from_slice(&[7; 16]);
        let commit = CtrlMessage::PairCommit {
            transcript,
            owner_proof: crate::protocol_v2::PairProof {
                mac: [0; 32],
                signature: [0; 64],
            },
            redeemer_proof: crate::protocol_v2::PairProof {
                mac: [0; 32],
                signature: [0; 64],
            },
            owner_prepared_signature: [0; 64],
            redeemer_prepared_signature: [0; 64],
            commit_revision: 3,
        };
        let keys = routing_keys(&commit);
        assert_eq!(keys, vec![DispatchKey::Pairing([7; 16])]);
    }
}
