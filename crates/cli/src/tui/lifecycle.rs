//! The rules for what a Session is doing and what stopping means, kept apart from the screen.
//!
//! Everything here is a pure function of its inputs, so each rule can be tested without a
//! terminal or an agent. The App feeds in what happened and carries out what comes back.

/// What a Session is doing, which decides how the person can interact with it.
///
/// ```text
///                  turn started
///        +------------------------------+
///        |                              v
///    +--------+    turn ended      +---------+
///    |  Idle  | <----------------- | Working |
///    +--------+                    +---------+
///        ^                           ^     |
///        |              approval     |     | approval
///        |              answered     |     | requested
///        |                           |     v
///        |    turn ended     +------------------+
///        +------------------ | AwaitingApproval |
///                            +------------------+
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing is running, and the Session is waiting for the person's next input.
    Idle,
    /// A Turn is running without needing the person.
    Working,
    /// A Turn cannot continue until the person answers about a shell command.
    AwaitingApproval,
}

/// Something that moves a Session between [`SessionState`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// A Turn was started.
    TurnStarted,
    /// The running Turn asked the person about a shell command.
    ApprovalRequested,
    /// The person answered about a shell command.
    ApprovalAnswered,
    /// The running Turn ended, however that came about.
    TurnEnded,
}

impl SessionState {
    /// Returns the state after `event`. An event that means nothing in this state changes nothing.
    #[must_use]
    pub fn after(self, event: SessionEvent) -> Self {
        match self {
            Self::Idle => Self::after_idle(event),
            Self::Working => Self::after_working(event),
            Self::AwaitingApproval => Self::after_awaiting_approval(event),
        }
    }

    /// Returns what an idle Session does on `event`.
    fn after_idle(event: SessionEvent) -> Self {
        match event {
            SessionEvent::TurnStarted => Self::Working,
            _ => Self::Idle,
        }
    }

    /// Returns what a working Session does on `event`.
    fn after_working(event: SessionEvent) -> Self {
        match event {
            SessionEvent::ApprovalRequested => Self::AwaitingApproval,
            SessionEvent::TurnEnded => Self::Idle,
            _ => Self::Working,
        }
    }

    /// Returns what a Session waiting on the person does on `event`.
    fn after_awaiting_approval(event: SessionEvent) -> Self {
        match event {
            SessionEvent::ApprovalAnswered => Self::Working,
            SessionEvent::TurnEnded => Self::Idle,
            _ => Self::AwaitingApproval,
        }
    }

    /// Returns whether a Turn has been started and has not yet ended.
    #[must_use]
    pub fn in_turn(self) -> bool {
        self != Self::Idle
    }

    /// Returns whether the Session is held up by the person rather than by the agent.
    #[must_use]
    pub fn is_waiting(self) -> bool {
        self != Self::Working
    }
}

/// What Alt-C does right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    /// Cancels the Turn on screen.
    CancelTurn,
    /// Stops the harness.
    Quit,
    /// Does nothing but say why, because quitting would stop work the person cannot see.
    RefuseToQuit,
}

/// Decides what Alt-C does. It only ever affects the Session on screen, whose state is `shown`,
/// so that one stray press cannot stop work the person is not looking at. `others_in_turn` says
/// whether any other Session has a Turn running.
#[must_use]
pub fn interrupt(shown: SessionState, others_in_turn: bool) -> Interrupt {
    if shown.in_turn() {
        Interrupt::CancelTurn
    } else if others_in_turn {
        Interrupt::RefuseToQuit
    } else {
        Interrupt::Quit
    }
}

#[cfg(test)]
mod tests {
    use super::SessionEvent::{ApprovalAnswered, ApprovalRequested, TurnEnded, TurnStarted};
    use super::SessionState::{AwaitingApproval, Idle, Working};
    use super::*;

    #[test]
    fn sessions_follow_their_transitions() {
        let transitions = [
            (Idle, TurnStarted, Working),
            (Working, ApprovalRequested, AwaitingApproval),
            (Working, TurnEnded, Idle),
            (AwaitingApproval, ApprovalAnswered, Working),
            (AwaitingApproval, TurnEnded, Idle),
        ];
        for (from, event, to) in transitions {
            assert_eq!(from.after(event), to, "{from:?} on {event:?}");
        }
    }

    #[test]
    fn events_that_mean_nothing_change_nothing() {
        let ignored = [
            (Idle, ApprovalRequested),
            (Idle, ApprovalAnswered),
            (Idle, TurnEnded),
            (Working, TurnStarted),
            (Working, ApprovalAnswered),
            (AwaitingApproval, TurnStarted),
            (AwaitingApproval, ApprovalRequested),
        ];
        for (state, event) in ignored {
            assert_eq!(state.after(event), state, "{state:?} on {event:?}");
        }
    }

    #[test]
    fn a_turn_lasts_until_it_ends_and_only_the_person_can_be_waited_on() {
        assert!(!Idle.in_turn());
        assert!(Working.in_turn());
        assert!(AwaitingApproval.in_turn());
        assert!(Idle.is_waiting());
        assert!(!Working.is_waiting());
        assert!(AwaitingApproval.is_waiting());
    }

    #[test]
    fn ctrl_c_only_ever_affects_the_session_on_screen() {
        assert_eq!(interrupt(Working, false), Interrupt::CancelTurn);
        assert_eq!(interrupt(Working, true), Interrupt::CancelTurn);
        assert_eq!(interrupt(AwaitingApproval, true), Interrupt::CancelTurn);
        assert_eq!(interrupt(Idle, false), Interrupt::Quit);
        assert_eq!(interrupt(Idle, true), Interrupt::RefuseToQuit);
    }
}
