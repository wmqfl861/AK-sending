//! Receiver-side task lifecycle model only: no disk persistence or scheduler.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending, Connecting, Transferring, WaitingDirect, Paused,
    Verifying, Saving, Completed, Cancelled, Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Begin, DirectReady, DirectUnavailable, Pause, Resume,
    AllBytesReceived, VerificationPassed, SaveCommitted, Cancel, Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTransition { pub state: State, pub event: Event }

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid task transition: {:?} + {:?}", self.state, self.event)
    }
}
impl std::error::Error for InvalidTransition {}

impl State {
    pub fn advance(self, event: Event) -> Result<Self, InvalidTransition> {
        let invalid = InvalidTransition { state: self, event };
        if matches!(self, Self::Completed | Self::Cancelled | Self::Failed) {
            return Err(invalid);
        }
        match (self, event) {
            (_, Event::Cancel) => Ok(Self::Cancelled),
            (_, Event::Fail) => Ok(Self::Failed),
            (Self::Pending, Event::Begin) => Ok(Self::Connecting),
            (Self::Connecting, Event::DirectReady) => Ok(Self::Transferring),
            (Self::Connecting | Self::Transferring, Event::DirectUnavailable) => Ok(Self::WaitingDirect),
            (Self::Connecting | Self::Transferring | Self::WaitingDirect, Event::Pause) => Ok(Self::Paused),
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

    #[test]
    fn success_requires_verification_then_save() {
        let events = [Event::Begin, Event::DirectReady, Event::AllBytesReceived,
                      Event::VerificationPassed, Event::SaveCommitted];
        let mut state = State::Pending;
        for event in events { state = state.advance(event).unwrap(); }
        assert_eq!(state, State::Completed);
    }

    #[test]
    fn bytes_received_is_not_completion() {
        assert_eq!(State::Transferring.advance(Event::AllBytesReceived).unwrap(), State::Verifying);
        assert!(State::Verifying.advance(Event::SaveCommitted).is_err());
        assert!(State::Transferring.advance(Event::SaveCommitted).is_err());
    }

    #[test]
    fn connection_loss_waits_without_relay_fallback() {
        let state = State::Transferring.advance(Event::DirectUnavailable).unwrap();
        assert_eq!(state, State::WaitingDirect);
        assert_eq!(state.advance(Event::Resume).unwrap(), State::Connecting);
    }

    #[test]
    fn pause_resume_rechecks_connection() {
        let state = State::Transferring.advance(Event::Pause).unwrap();
        assert_eq!(state, State::Paused);
        assert_eq!(state.advance(Event::Resume).unwrap(), State::Connecting);
        assert!(state.advance(Event::DirectReady).is_err());
    }

    #[test]
    fn terminal_states_do_not_restart_or_accept_success() {
        for state in [State::Completed, State::Cancelled, State::Failed] {
            for event in [Event::Begin, Event::Resume, Event::SaveCommitted, Event::Cancel] {
                assert!(state.advance(event).is_err());
            }
        }
    }

    #[test]
    fn invalid_transition_reports_state_and_event() {
        let e = State::Pending.advance(Event::SaveCommitted).unwrap_err();
        assert_eq!(e, InvalidTransition { state: State::Pending, event: Event::SaveCommitted });
        assert!(e.to_string().contains("Pending"));
    }
}
