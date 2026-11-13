//! What one open Session is doing, and what the person's input to it comes to.
//!
//! The [`RunningHarness`] applies these to every open Session, and the
//! [`SessionController`](super::SessionController) of each keeps its state.
//!
//! [`RunningHarness`]: crate::running_harness::RunningHarness

use std::path::PathBuf;

use crate::commands::Command;
use crate::workspace::Checkout;

/// What a Session is doing, which decides how the person can interact with it.
///
/// A Turn awaiting an answer parks when a Rebuild puts off what it asked.
///
/// ```text
///                  turn started
///        +------------------------------+
///        |                              v
///    +--------+    turn ended      +---------+  parked   +--------+
///    |  Idle  | <----------------- | Working | --------> | Parked |
///    +--------+                    +---------+ <-------- +--------+
///        ^                           ^     |    continued or   ^
///        |                           |     |    turn started   |
///        |              answered     |     | asked             |
///        |                           |     |                   |
///        |    turn ended     +----------------+   parked       |
///        +------------------ | AwaitingAnswer | ---------------+
///                            +----------------+
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing is running, and the Session is waiting for the person's next input.
    Idle,
    /// A Turn is running without needing the person.
    Working,
    /// A Turn cannot continue until the person answers a question: whether a shell command may
    /// run, or the model's Questions. A Rebuild puts the question off, which parks the Turn.
    AwaitingAnswer,
    /// A Turn stopped so that the Running Harness could stop, and will carry on. It stopped
    /// between Steps, or at a question that a Rebuild put off, which it asks again.
    Parked,
}

/// Something that moves a Session between [`SessionState`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// A Turn was started.
    TurnStarted,
    /// The running Turn asked the person whether a shell command may run, or put Questions to
    /// them.
    Asked,
    /// The person answered what the running Turn asked.
    Answered,
    /// The running Turn ended, however that came about.
    TurnEnded,
    /// The running Turn stopped to carry on later, either between Steps as asked or at a question
    /// that a Rebuild put off.
    Parked,
    /// A parked Turn was carried on.
    Continued,
}

impl SessionState {
    /// Returns the state after `event`. An event that means nothing in this state changes nothing.
    #[must_use]
    pub fn after(self, event: SessionEvent) -> Self {
        match self {
            Self::Idle => Self::after_idle(event),
            Self::Working => Self::after_working(event),
            Self::AwaitingAnswer => Self::after_awaiting_answer(event),
            Self::Parked => Self::after_parked(event),
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
            SessionEvent::Asked => Self::AwaitingAnswer,
            SessionEvent::TurnEnded => Self::Idle,
            SessionEvent::Parked => Self::Parked,
            _ => Self::Working,
        }
    }

    /// Returns what a Session waiting on the person does on `event`.
    fn after_awaiting_answer(event: SessionEvent) -> Self {
        match event {
            SessionEvent::Answered => Self::Working,
            SessionEvent::TurnEnded => Self::Idle,
            SessionEvent::Parked => Self::Parked,
            _ => Self::AwaitingAnswer,
        }
    }

    /// Returns what a parked Session does on `event`. The person can start a new Turn on one that
    /// could not be carried on.
    fn after_parked(event: SessionEvent) -> Self {
        match event {
            SessionEvent::Continued | SessionEvent::TurnStarted => Self::Working,
            _ => Self::Parked,
        }
    }

    /// Returns whether a Turn is running, which a Rebuild or a quit has to wait for. A Rebuild
    /// first puts off every question, so a Turn waiting on the person parks at once rather than
    /// wait for them.
    #[must_use]
    pub fn in_turn(self) -> bool {
        matches!(self, Self::Working | Self::AwaitingAnswer)
    }

    /// Returns whether the Session is held up by the person rather than by the agent.
    #[must_use]
    pub fn is_waiting(self) -> bool {
        matches!(self, Self::Idle | Self::AwaitingAnswer)
    }
}

/// What the interface has to do after the [`RunningHarness`] has acted, or may want to know.
///
/// [`RunningHarness`]: crate::running_harness::RunningHarness
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<'a> {
    /// A Turn started in the focused Session on this text. The interface can use it at once, for
    /// instance to name the Session, before the Turn itself reports.
    TurnStarted(String),
    /// The prompt became a Queued Prompt, because a Turn is running in the focused Session. The
    /// Turn takes it at the end of its current Step, or the next Turn starts on it if this one
    /// ends first, which then comes back as [`Outcome::TurnStarted`].
    Queued,
    /// Queued Prompts that will not be sent, joined into this text, which the interface puts
    /// back into the focused Session's Draft so that nothing typed is lost. They come back when
    /// the person interrupts the Turn they were queued for, or when it ends during a drain.
    Withdrawn(String),
    /// Nothing happened, for this reason, which the interface tells the person in its own words.
    Refused(Refusal),
    /// A Human Command that only the interface can carry out, because it is about how the
    /// interface keeps its Sessions: [`Command::New`], [`Command::NewMain`],
    /// [`Command::Archive`], [`Command::Vim`], [`Command::Rename`] or [`Command::Connection`]. No
    /// other comes back.
    Command(Command<'a>),
    /// A request to start building this checkout of the Harness Source for a Rebuild, whose
    /// result the interface passes to [`RunningHarness::build_finished`]. A
    /// [`Checkout::Worktree`] means the worktree of the Session the person asked from.
    ///
    /// [`RunningHarness::build_finished`]: crate::running_harness::RunningHarness::build_finished
    StartBuild(Checkout),
    /// A request to stop the build, if one is running.
    CancelBuild,
    /// The end of the Running Harness.
    Exit(Exit),
}

/// Why the [`RunningHarness`] did not do what the person asked.
///
/// [`RunningHarness`]: crate::running_harness::RunningHarness
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A Turn is running in the Session, which has to end first.
    Busy,
    /// The Running Harness is on its way out, so nothing new starts.
    Draining,
    /// Quitting would stop Turns in other Sessions, which the person cannot see.
    OthersWorking,
    /// This slash-prefixed line names neither a Human Command nor a Skill.
    UnknownCommand(String),
    /// A Skill was asked for by a bare name that several Skills share.
    AmbiguousSkill {
        /// The name asked for.
        name: String,
        /// The full names of the Skills it could mean.
        names: Vec<String>,
    },
    /// A Skill was found but could not be read.
    UnreadableSkill {
        /// The name asked for.
        name: String,
        /// What went wrong.
        error: String,
    },
}

/// Why the Running Harness stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// The person is finished.
    Quit,
    /// The Rebuild is ready: the Running Harness should relaunch into this binary. The interface
    /// can carry on if that fails.
    Rebuild(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::SessionEvent::{Answered, Asked, Continued, TurnEnded, TurnStarted};
    use super::SessionState::{AwaitingAnswer, Idle, Parked, Working};
    use super::*;

    #[test]
    fn sessions_follow_their_transitions() {
        let transitions = [
            (Idle, TurnStarted, Working),
            (Working, Asked, AwaitingAnswer),
            (Working, TurnEnded, Idle),
            (AwaitingAnswer, Answered, Working),
            (AwaitingAnswer, TurnEnded, Idle),
            (AwaitingAnswer, SessionEvent::Parked, Parked),
            (Working, SessionEvent::Parked, Parked),
            (Parked, Continued, Working),
            (Parked, TurnStarted, Working),
        ];
        for (from, event, to) in transitions {
            assert_eq!(from.after(event), to, "{from:?} on {event:?}");
        }
    }

    #[test]
    fn events_that_mean_nothing_change_nothing() {
        let ignored = [
            (Idle, Asked),
            (Idle, Answered),
            (Idle, TurnEnded),
            (Working, TurnStarted),
            (Working, Answered),
            (AwaitingAnswer, TurnStarted),
            (AwaitingAnswer, Asked),
            (Idle, SessionEvent::Parked),
            (Parked, TurnEnded),
        ];
        for (state, event) in ignored {
            assert_eq!(state.after(event), state, "{state:?} on {event:?}");
        }
    }

    #[test]
    fn a_turn_lasts_until_it_ends_and_only_the_person_can_be_waited_on() {
        assert!(!Idle.in_turn());
        assert!(Working.in_turn());
        assert!(AwaitingAnswer.in_turn());
        assert!(!Parked.in_turn());
        assert!(Idle.is_waiting());
        assert!(!Working.is_waiting());
        assert!(AwaitingAnswer.is_waiting());
        assert!(!Parked.is_waiting());
    }
}
