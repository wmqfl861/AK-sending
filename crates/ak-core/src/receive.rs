//! Receiver-local consent control. No network, disk, keys or cryptographic claims.
//!
//! Only the trusted receiver UI/policy may call `approve`. Never deserialize a
//! sender message into that call. The transport module must independently prove
//! the peer and direct path. Check permission before each write batch and commit;
//! a successful check is not a lifetime capability. Mutable access serializes
//! cancellation, but future asynchronous I/O must also fence in-flight writes.
//!
//! State and approvals cannot be forged through the public safe API:
//! ```compile_fail
//! use ak_core::{job::State, receive::ReceiveJob};
//! let mut job = ReceiveJob::new([1; 16], [2; 32]);
//! job.state = State::Transferring;
//! ```
use crate::job::{Event, InvalidTransition, State};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Opaque, process-local reference for a receiver confirmation dialog.
/// Cloning a request does not clone a permission. Not serializable or persistent.
#[derive(Debug, Clone)]
pub struct ConsentRequest {
    task_id: [u8; 16],
    peer_id: [u8; 32],
    generation: u64,
    scope: Arc<()>,
    expires_at: Instant,
}

impl ConsentRequest {
    pub fn task_id(&self) -> [u8; 16] {
        self.task_id
    }
    pub fn peer_id(&self) -> [u8; 32] {
        self.peer_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn matches(&self, other: &Self) -> bool {
        self.task_id == other.task_id
            && self.peer_id == other.peer_id
            && self.generation == other.generation
            && Arc::ptr_eq(&self.scope, &other.scope)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveError {
    Transition(InvalidTransition),
    WrongPeer,
    StaleRequest,
    InvalidLifetime,
    GenerationExhausted,
    PermissionRequired,
    PermissionExpired,
    RequestExpired,
    WrongStage,
}

impl fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receive authorization error: {self:?}")
    }
}
impl std::error::Error for ReceiveError {}
impl From<InvalidTransition> for ReceiveError {
    fn from(value: InvalidTransition) -> Self {
        Self::Transition(value)
    }
}

#[derive(Debug)]
struct Permission {
    expires_at: Instant,
}

// Confirmation dialogs expire independently of the approved transfer lifetime.
const CONFIRMATION_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy)]
enum Phase {
    Receiving,
    Verifying,
    Saving,
}

impl Phase {
    fn grant_event(self) -> Event {
        match self {
            Self::Receiving => Event::ConsentGranted,
            Self::Verifying => Event::ConsentGrantedForVerification,
            Self::Saving => Event::ConsentGrantedForSaving,
        }
    }
}

/// One receiver task. Not Clone, Copy or serializable. Constructing a replacement
/// with identical identifiers does not make old UI requests valid for it.
#[derive(Debug)]
pub struct ReceiveJob {
    task_id: [u8; 16],
    expected_peer: [u8; 32],
    generation: u64,
    state: State,
    phase: Phase,
    request: Option<ConsentRequest>,
    permission: Option<Permission>,
}

impl ReceiveJob {
    pub fn new(task_id: [u8; 16], expected_peer: [u8; 32]) -> Self {
        Self {
            task_id,
            expected_peer,
            generation: 0,
            state: State::Pending,
            phase: Phase::Receiving,
            request: None,
            permission: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn begin(&mut self) -> Result<(), ReceiveError> {
        self.transition(Event::Begin)
    }

    /// Called ONLY by a trusted adapter after real connection verification.
    /// Comparing this peer identifier is NOT cryptographic verification.
    pub fn direct_ready(&mut self, verified_peer: [u8; 32]) -> Result<(), ReceiveError> {
        self.direct_ready_at(verified_peer, Instant::now())
    }

    fn direct_ready_at(
        &mut self,
        verified_peer: [u8; 32],
        now: Instant,
    ) -> Result<(), ReceiveError> {
        if verified_peer != self.expected_peer {
            return Err(ReceiveError::WrongPeer);
        }
        let next = self.state.advance(Event::DirectReady)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ReceiveError::GenerationExhausted)?;
        let expires_at = now
            .checked_add(CONFIRMATION_LIFETIME)
            .ok_or(ReceiveError::InvalidLifetime)?;
        self.request = Some(ConsentRequest {
            task_id: self.task_id,
            peer_id: verified_peer,
            generation,
            scope: Arc::new(()),
            expires_at,
        });
        self.generation = generation;
        self.permission = None;
        self.state = next;
        Ok(())
    }

    pub fn consent_request(&self) -> Option<ConsentRequest> {
        if self.state == State::AwaitingConsent {
            self.request.clone()
        } else {
            None
        }
    }

    /// Local user/policy approval. The monotonic timestamp cannot be supplied by
    /// the network or by a public caller. Zero and overflowing lifetimes fail.
    pub fn approve(
        &mut self,
        request: &ConsentRequest,
        lifetime: Duration,
    ) -> Result<(), ReceiveError> {
        self.approve_at(request, lifetime, Instant::now())
    }

    fn approve_at(
        &mut self,
        request: &ConsentRequest,
        lifetime: Duration,
        now: Instant,
    ) -> Result<(), ReceiveError> {
        let next = self.state.advance(self.phase.grant_event())?;
        self.match_request(request)?;
        if now >= request.expires_at {
            self.transition(Event::ConsentRevoked)?;
            return Err(ReceiveError::RequestExpired);
        }
        if lifetime.is_zero() {
            return Err(ReceiveError::InvalidLifetime);
        }
        let expires_at = now
            .checked_add(lifetime)
            .ok_or(ReceiveError::InvalidLifetime)?;
        self.permission = Some(Permission { expires_at });
        self.state = next;
        Ok(())
    }

    pub fn reject(&mut self, request: &ConsentRequest) -> Result<(), ReceiveError> {
        let next = self.state.advance(Event::ConsentDenied)?;
        self.match_request(request)?;
        self.state = next;
        self.clear_permission();
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), ReceiveError> {
        self.transition(Event::Pause)
    }
    pub fn resume(&mut self) -> Result<(), ReceiveError> {
        self.transition(Event::Resume)
    }
    pub fn connection_lost(&mut self) -> Result<(), ReceiveError> {
        self.transition(Event::DirectUnavailable)
    }
    pub fn revoke(&mut self) -> Result<(), ReceiveError> {
        self.transition(Event::ConsentRevoked)
    }
    pub fn cancel(&mut self) -> Result<(), ReceiveError> {
        self.transition(Event::Cancel)
    }
    pub fn fail(&mut self) -> Result<(), ReceiveError> {
        self.transition(Event::Fail)
    }

    /// Must be checked before every bounded write batch, not just at startup.
    pub fn check_transfer(&mut self) -> Result<(), ReceiveError> {
        self.check_at(State::Transferring, Instant::now())
    }

    pub fn all_bytes_received(&mut self) -> Result<(), ReceiveError> {
        self.advance_authorized(Event::AllBytesReceived, State::Transferring, Instant::now())
    }

    pub fn verification_passed(&mut self) -> Result<(), ReceiveError> {
        self.advance_authorized(Event::VerificationPassed, State::Verifying, Instant::now())
    }

    /// Call only after a reliable save commit; this module does not write disks.
    pub fn save_committed(&mut self) -> Result<(), ReceiveError> {
        self.advance_authorized(Event::SaveCommitted, State::Saving, Instant::now())
    }

    fn match_request(&self, request: &ConsentRequest) -> Result<(), ReceiveError> {
        if self
            .request
            .as_ref()
            .is_some_and(|current| current.matches(request))
        {
            Ok(())
        } else {
            Err(ReceiveError::StaleRequest)
        }
    }

    fn clear_permission(&mut self) {
        self.request = None;
        self.permission = None;
    }

    fn transition(&mut self, event: Event) -> Result<(), ReceiveError> {
        let next = self.state.advance(event)?;
        self.clear_permission();
        self.state = next;
        Ok(())
    }

    fn check_at(&mut self, expected: State, now: Instant) -> Result<(), ReceiveError> {
        if self.state != expected {
            return Err(ReceiveError::WrongStage);
        }
        let permission = self
            .permission
            .as_ref()
            .ok_or(ReceiveError::PermissionRequired)?;
        if now >= permission.expires_at {
            self.transition(Event::ConsentRevoked)?;
            return Err(ReceiveError::PermissionExpired);
        }
        Ok(())
    }

    fn advance_authorized(
        &mut self,
        event: Event,
        expected: State,
        now: Instant,
    ) -> Result<(), ReceiveError> {
        self.check_at(expected, now)?;
        let next = self.state.advance(event)?;
        self.state = next;
        match next {
            State::Verifying => self.phase = Phase::Verifying,
            State::Saving => self.phase = Phase::Saving,
            _ => {}
        }
        if next.is_terminal() {
            self.clear_permission();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TASK: [u8; 16] = [1; 16];
    const PEER: [u8; 32] = [2; 32];
    const TTL: Duration = Duration::from_secs(60);

    fn pending() -> (ReceiveJob, ConsentRequest) {
        pending_at(Instant::now())
    }
    fn pending_at(now: Instant) -> (ReceiveJob, ConsentRequest) {
        let mut j = ReceiveJob::new(TASK, PEER);
        j.begin().unwrap();
        j.direct_ready_at(PEER, now).unwrap();
        let r = j.consent_request().unwrap();
        (j, r)
    }
    fn approved(now: Instant) -> ReceiveJob {
        let (mut j, r) = pending_at(now);
        j.approve_at(&r, TTL, now).unwrap();
        j
    }

    #[test]
    fn direct_ready_alone_cannot_receive() {
        let (mut j, _) = pending();
        assert_eq!(j.check_transfer(), Err(ReceiveError::WrongStage));
        assert_eq!(j.all_bytes_received(), Err(ReceiveError::WrongStage));
        assert_eq!(j.state(), State::AwaitingConsent);
    }
    #[test]
    fn wrong_peer_does_not_change_state() {
        let mut j = ReceiveJob::new(TASK, PEER);
        j.begin().unwrap();
        assert_eq!(j.direct_ready([9; 32]), Err(ReceiveError::WrongPeer));
        assert_eq!(j.state(), State::Connecting);
        assert!(j.consent_request().is_none());
    }
    #[test]
    fn request_identifies_task_peer_and_generation() {
        let (_, r) = pending();
        assert_eq!(r.task_id(), TASK);
        assert_eq!(r.peer_id(), PEER);
        assert_eq!(r.generation(), 1);
    }
    #[test]
    fn old_dialog_from_same_ids_but_new_controller_is_rejected() {
        let (_, old) = pending();
        let (mut new, _) = pending();
        assert_eq!(new.approve(&old, TTL), Err(ReceiveError::StaleRequest));
        assert_eq!(new.reject(&old), Err(ReceiveError::StaleRequest));
        assert_eq!(new.state(), State::AwaitingConsent);
    }
    #[test]
    fn wrong_task_and_peer_requests_cannot_authorize() {
        for (task, peer) in [([9; 16], PEER), (TASK, [9; 32])] {
            let (mut j, _) = pending();
            let mut other = ReceiveJob::new(task, peer);
            other.begin().unwrap();
            other.direct_ready(peer).unwrap();
            assert_eq!(
                j.approve(&other.consent_request().unwrap(), TTL),
                Err(ReceiveError::StaleRequest)
            );
        }
    }
    #[test]
    fn stale_dialog_after_reconnect_cannot_authorize() {
        let (mut j, old) = pending();
        j.connection_lost().unwrap();
        j.resume().unwrap();
        j.direct_ready(PEER).unwrap();
        assert_eq!(j.approve(&old, TTL), Err(ReceiveError::StaleRequest));
        let new = j.consent_request().unwrap();
        assert_eq!(new.generation(), 2);
        j.approve(&new, TTL).unwrap();
        assert!(j.check_transfer().is_ok());
    }
    #[test]
    fn zero_and_overflowing_ttl_leave_request_pending() {
        let (mut j, r) = pending();
        for ttl in [Duration::ZERO, Duration::MAX] {
            assert_eq!(j.approve(&r, ttl), Err(ReceiveError::InvalidLifetime));
            assert_eq!(j.state(), State::AwaitingConsent);
        }
        j.approve(&r, TTL).unwrap();
    }
    #[test]
    fn exact_deadline_expires_and_requires_fresh_connection() {
        let now = Instant::now();
        let mut j = approved(now);
        assert_eq!(
            j.check_at(State::Transferring, now + TTL),
            Err(ReceiveError::PermissionExpired)
        );
        assert_eq!(j.state(), State::Paused);
        assert!(j.permission.is_none());
        assert!(j.consent_request().is_none());
        j.resume().unwrap();
        assert_eq!(j.check_transfer(), Err(ReceiveError::WrongStage));
    }
    #[test]
    fn just_before_deadline_is_allowed() {
        let now = Instant::now();
        let mut j = approved(now);
        assert!(j
            .check_at(State::Transferring, now + TTL - Duration::from_nanos(1))
            .is_ok());
    }
    #[test]
    fn expiry_during_verify_or_save_prevents_completion() {
        for stage in [State::Verifying, State::Saving] {
            let now = Instant::now();
            let mut j = approved(now);
            j.advance_authorized(Event::AllBytesReceived, State::Transferring, now)
                .unwrap();
            if stage == State::Saving {
                j.advance_authorized(Event::VerificationPassed, State::Verifying, now)
                    .unwrap();
            }
            let event = if stage == State::Saving {
                Event::SaveCommitted
            } else {
                Event::VerificationPassed
            };
            assert_eq!(
                j.advance_authorized(event, stage, now + TTL),
                Err(ReceiveError::PermissionExpired)
            );
            assert_eq!(j.state(), State::Paused);
        }
    }
    #[test]
    fn interruptions_clear_permission_in_all_active_stages() {
        for stage in [State::Transferring, State::Verifying, State::Saving] {
            for event in [
                Event::Pause,
                Event::ConsentRevoked,
                Event::DirectUnavailable,
                Event::Cancel,
                Event::Fail,
            ] {
                let now = Instant::now();
                let mut j = approved(now);
                if stage != State::Transferring {
                    j.advance_authorized(Event::AllBytesReceived, State::Transferring, now)
                        .unwrap();
                }
                if stage == State::Saving {
                    j.advance_authorized(Event::VerificationPassed, State::Verifying, now)
                        .unwrap();
                }
                j.transition(event).unwrap();
                assert!(j.permission.is_none());
                assert!(j.request.is_none());
                assert!(j.check_transfer().is_err());
            }
        }
    }
    #[test]
    fn successful_save_is_terminal_and_clears_permission() {
        let mut j = approved(Instant::now());
        j.all_bytes_received().unwrap();
        j.verification_passed().unwrap();
        j.save_committed().unwrap();
        assert_eq!(j.state(), State::Completed);
        assert!(j.permission.is_none());
        assert!(j.resume().is_err());
        assert!(j.save_committed().is_err());
    }
    #[test]
    fn cannot_skip_verification_or_save() {
        let mut j = approved(Instant::now());
        assert_eq!(j.save_committed(), Err(ReceiveError::WrongStage));
        assert_eq!(j.state(), State::Transferring);
        j.all_bytes_received().unwrap();
        assert_eq!(j.save_committed(), Err(ReceiveError::WrongStage));
        assert_eq!(j.state(), State::Verifying);
    }
    #[test]
    fn duplicate_approval_is_rejected_without_removing_valid_permission() {
        let (mut j, r) = pending();
        j.approve(&r, TTL).unwrap();
        assert!(j.approve(&r, TTL).is_err());
        assert!(j.check_transfer().is_ok());
    }
    #[test]
    fn rejection_is_terminal() {
        let (mut j, r) = pending();
        j.reject(&r).unwrap();
        assert_eq!(j.state(), State::Rejected);
        assert!(j.approve(&r, TTL).is_err());
        assert!(j.resume().is_err());
        assert!(j.request.is_none());
    }
    #[test]
    fn generation_exhaustion_fails_without_state_mutation() {
        let mut j = ReceiveJob::new(TASK, PEER);
        j.begin().unwrap();
        j.generation = u64::MAX;
        assert_eq!(j.direct_ready(PEER), Err(ReceiveError::GenerationExhausted));
        assert_eq!(j.state(), State::Connecting);
        assert!(j.request.is_none());
    }
    #[test]
    fn permission_is_required_even_for_internally_inconsistent_state() {
        let mut j = approved(Instant::now());
        j.permission = None;
        assert_eq!(j.check_transfer(), Err(ReceiveError::PermissionRequired));
    }
    #[test]
    fn invalid_transition_keeps_authorization_intact() {
        let mut j = approved(Instant::now());
        assert!(j.begin().is_err());
        assert!(j.check_transfer().is_ok());
    }
    #[test]
    fn receiver_controller_and_dialog_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ReceiveJob>();
        assert_send_sync::<ConsentRequest>();
    }

    #[test]
    fn confirmation_request_expires_even_without_connection_change() {
        let now = Instant::now();
        let (mut j, r) = pending_at(now);
        let deadline = now + CONFIRMATION_LIFETIME;
        assert_eq!(
            j.approve_at(&r, TTL, deadline),
            Err(ReceiveError::RequestExpired)
        );
        assert_eq!(j.state(), State::Paused);
        assert!(j.request.is_none());
        assert!(j.permission.is_none());
        j.resume().unwrap();
        j.direct_ready_at(PEER, deadline).unwrap();
        assert_eq!(j.approve_at(&r, TTL, deadline), Err(ReceiveError::StaleRequest));
        let fresh = j.consent_request().unwrap();
        j.approve_at(&fresh, TTL, deadline).unwrap();
        assert_eq!(j.state(), State::Transferring);
    }

    #[test]
    fn confirmation_is_valid_just_before_its_own_deadline() {
        let now = Instant::now();
        let (mut j, r) = pending_at(now);
        let before = now + CONFIRMATION_LIFETIME - Duration::from_nanos(1);
        j.approve_at(&r, TTL, before).unwrap();
        assert!(j.check_at(State::Transferring, before).is_ok());
    }

    #[test]
    fn expired_permission_resumes_verification_or_save_not_receiving() {
        for stage in [State::Verifying, State::Saving] {
            let now = Instant::now();
            let mut j = approved(now);
            j.advance_authorized(Event::AllBytesReceived, State::Transferring, now)
                .unwrap();
            if stage == State::Saving {
                j.advance_authorized(Event::VerificationPassed, State::Verifying, now)
                    .unwrap();
            }
            let expired = now + TTL;
            assert_eq!(
                j.check_at(stage, expired),
                Err(ReceiveError::PermissionExpired)
            );
            j.resume().unwrap();
            j.direct_ready_at(PEER, expired).unwrap();
            let request = j.consent_request().unwrap();
            j.approve_at(&request, TTL, expired).unwrap();
            assert_eq!(j.state(), stage);
            assert_eq!(
                j.check_at(State::Transferring, expired),
                Err(ReceiveError::WrongStage)
            );
        }
    }
}
