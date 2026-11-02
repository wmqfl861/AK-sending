//! Pure transition model, not permission to write a file or evidence of authentication.
//! Production callers must additionally use the receiver-local consent controller
//! and, in the next transport module, an authenticated direct connection.
//! Resume-grant events are selected by the controller's private retained phase,
//! never by a network field or an untrusted caller.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending,
    Connecting,
    AwaitingConsent,
    Transferring,
    WaitingDirect,
    Paused,
    Verifying,
    Saving,
    Completed,
    Rejected,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Begin,
    DirectReady,
    ConsentGranted,
    ConsentGrantedForVerification,
    ConsentGrantedForSaving,
    ConsentDenied,
    ConsentRevoked,
    DirectUnavailable,
    Pause,
    Resume,
    AllBytesReceived,
    VerificationPassed,
    SaveCommitted,
    Cancel,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTransition {
    pub state: State,
    pub event: Event,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid task transition: {:?} + {:?}",
            self.state, self.event
        )
    }
}
impl std::error::Error for InvalidTransition {}

impl State {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Rejected | Self::Cancelled | Self::Failed
        )
    }

    pub fn advance(self, event: Event) -> Result<Self, InvalidTransition> {
        let invalid = InvalidTransition { state: self, event };
        if self.is_terminal() {
            return Err(invalid);
        }
        match (self, event) {
            (_, Event::Cancel) => Ok(Self::Cancelled),
            (_, Event::Fail) => Ok(Self::Failed),
            (Self::Pending, Event::Begin) => Ok(Self::Connecting),
            (Self::Connecting, Event::DirectReady) => Ok(Self::AwaitingConsent),
            (Self::AwaitingConsent, Event::ConsentGranted) => Ok(Self::Transferring),
            (Self::AwaitingConsent, Event::ConsentGrantedForVerification) => Ok(Self::Verifying),
            (Self::AwaitingConsent, Event::ConsentGrantedForSaving) => Ok(Self::Saving),
            (Self::AwaitingConsent, Event::ConsentDenied) => Ok(Self::Rejected),
            (
                Self::AwaitingConsent | Self::Transferring | Self::Verifying | Self::Saving,
                Event::ConsentRevoked,
            ) => Ok(Self::Paused),
            (
                Self::Connecting
                | Self::AwaitingConsent
                | Self::Transferring
                | Self::Verifying
                | Self::Saving,
                Event::DirectUnavailable,
            ) => Ok(Self::WaitingDirect),
            (
                Self::Connecting
                | Self::AwaitingConsent
                | Self::Transferring
                | Self::WaitingDirect
                | Self::Verifying
                | Self::Saving,
                Event::Pause,
            ) => Ok(Self::Paused),
            (Self::Paused | Self::WaitingDirect, Event::Resume) => Ok(Self::Connecting),
            (Self::Transferring, Event::AllBytesReceived) => Ok(Self::Verifying),
            (Self::Verifying, Event::VerificationPassed) => Ok(Self::Saving),
            (Self::Saving, Event::SaveCommitted) => Ok(Self::Completed),
            _ => Err(invalid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATES: [State; 12] = [
        State::Pending,
        State::Connecting,
        State::AwaitingConsent,
        State::Transferring,
        State::WaitingDirect,
        State::Paused,
        State::Verifying,
        State::Saving,
        State::Completed,
        State::Rejected,
        State::Cancelled,
        State::Failed,
    ];
    const EVENTS: [Event; 15] = [
        Event::Begin,
        Event::DirectReady,
        Event::ConsentGranted,
        Event::ConsentGrantedForVerification,
        Event::ConsentGrantedForSaving,
        Event::ConsentDenied,
        Event::ConsentRevoked,
        Event::DirectUnavailable,
        Event::Pause,
        Event::Resume,
        Event::AllBytesReceived,
        Event::VerificationPassed,
        Event::SaveCommitted,
        Event::Cancel,
        Event::Fail,
    ];

    #[test]
    fn success_requires_consent_verification_and_save() {
        let mut s = State::Pending;
        for e in [
            Event::Begin,
            Event::DirectReady,
            Event::ConsentGranted,
            Event::AllBytesReceived,
            Event::VerificationPassed,
            Event::SaveCommitted,
        ] {
            s = s.advance(e).unwrap();
        }
        assert_eq!(s, State::Completed);
    }

    #[test]
    fn direct_ready_never_means_consent() {
        assert_eq!(
            State::Connecting.advance(Event::DirectReady),
            Ok(State::AwaitingConsent)
        );
        assert!(State::AwaitingConsent
            .advance(Event::AllBytesReceived)
            .is_err());
        assert_eq!(
            State::AwaitingConsent.advance(Event::ConsentDenied),
            Ok(State::Rejected)
        );
    }

    #[test]
    fn terminal_states_reject_all_events() {
        for s in STATES.into_iter().filter(|s| s.is_terminal()) {
            for e in EVENTS {
                assert_eq!(s.advance(e), Err(InvalidTransition { state: s, event: e }));
            }
        }
    }

    #[test]
    fn every_transition_into_transfer_requires_pending_consent() {
        for s in STATES {
            for e in EVENTS {
                if s.advance(e) == Ok(State::Transferring) {
                    assert_eq!((s, e), (State::AwaitingConsent, Event::ConsentGranted));
                }
            }
        }
    }

    #[test]
    fn every_transition_into_completed_requires_save_ack() {
        for s in STATES {
            for e in EVENTS {
                if s.advance(e) == Ok(State::Completed) {
                    assert_eq!((s, e), (State::Saving, Event::SaveCommitted));
                }
            }
        }
    }

    #[test]
    fn loss_and_pause_resume_recheck_connection() {
        for s in [
            State::AwaitingConsent,
            State::Transferring,
            State::Verifying,
            State::Saving,
        ] {
            assert_eq!(
                s.advance(Event::DirectUnavailable),
                Ok(State::WaitingDirect)
            );
            assert_eq!(s.advance(Event::Pause), Ok(State::Paused));
            assert_eq!(s.advance(Event::ConsentRevoked), Ok(State::Paused));
        }
        for s in [State::Paused, State::WaitingDirect] {
            assert_eq!(s.advance(Event::Resume), Ok(State::Connecting));
            assert!(s.advance(Event::ConsentGranted).is_err());
        }
    }

    #[test]
    fn invalid_transition_reports_context() {
        let e = State::Pending.advance(Event::SaveCommitted).unwrap_err();
        assert_eq!(
            e,
            InvalidTransition {
                state: State::Pending,
                event: Event::SaveCommitted
            }
        );
        assert!(e.to_string().contains("Pending"));
    }
}
