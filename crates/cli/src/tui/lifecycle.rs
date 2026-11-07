//! The rules for what a Session is doing and what stopping means, kept apart from the screen.
//!
//! Everything here is a pure function of its inputs, so each rule can be tested without a
//! terminal or an agent. The App feeds in what happened and carries out what comes back.

use std::path::PathBuf;

/// What a Session is doing, which decides how the person can interact with it.
///
/// ```text
///                  turn started
///        +------------------------------+
///        |                              v
///    +--------+    turn ended      +---------+  parked   +--------+
///    |  Idle  | <----------------- | Working | --------> | Parked |
///    +--------+                    +---------+ <-------- +--------+
///        ^                           ^     |    continued or
///        |              approval     |     |    turn started
///        |              answered     |     | approval
///        |                           |     | requested
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
    /// A Turn stopped between Steps so that the harness could stop, and will carry on.
    Parked,
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
    /// The running Turn stopped between Steps, as asked.
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
            Self::AwaitingApproval => Self::after_awaiting_approval(event),
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
            SessionEvent::ApprovalRequested => Self::AwaitingApproval,
            SessionEvent::TurnEnded => Self::Idle,
            SessionEvent::Parked => Self::Parked,
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

    /// Returns what a parked Session does on `event`. The person can start a new Turn on one that
    /// could not be carried on.
    fn after_parked(event: SessionEvent) -> Self {
        match event {
            SessionEvent::Continued | SessionEvent::TurnStarted => Self::Working,
            _ => Self::Parked,
        }
    }

    /// Returns whether a Turn is running, which a Rebuild or a quit has to wait for.
    #[must_use]
    pub fn in_turn(self) -> bool {
        matches!(self, Self::Working | Self::AwaitingApproval)
    }

    /// Returns whether the Session is held up by the person rather than by the agent.
    #[must_use]
    pub fn is_waiting(self) -> bool {
        matches!(self, Self::Idle | Self::AwaitingApproval)
    }
}

/// Whether the harness is carrying on or on its way out.
///
/// Every Session's Turns stop at the end of their Step while draining, so that nothing is cut
/// short. Parked Turns carry on afterwards, in this process if the drain is abandoned, or in the
/// next one.
///
/// ```text
///                     /quit
///        +--------------------------------+
///        |                                v
///   +---------+   Alt-C, or           +----------------+  all settled,
///   |         | <-------------------- | DrainingToQuit |  or /quit again
///   |         |                       +----------------+ ---------------> exit
///   | Running |
///   |         |   /rebuild            +-------------------+  all settled,
///   |         | --------------------> | DrainingToRebuild |  and built
///   |         | <-------------------- +-------------------+ -----------> relaunch
///   +---------+   Alt-C, or build failed
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Phase {
    /// Sessions work as normal.
    #[default]
    Running,
    /// The person is quitting, once Turns reach the end of their Step.
    DrainingToQuit,
    /// The Harness Source is building, and the harness relaunches once that has worked and Turns
    /// have reached the end of their Step.
    DrainingToRebuild {
        /// The new binary, once the build has finished.
        binary: Option<PathBuf>,
    },
}

/// Something that moves the harness between [`Phase`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// The person asked for a Rebuild.
    RebuildAsked,
    /// The person asked to quit.
    QuitAsked,
    /// No Session has a Turn running.
    Settled,
    /// The build finished, with the new binary if it worked.
    BuildFinished(Option<PathBuf>),
    /// The person asked to stop what the harness is doing.
    Interrupted,
}

/// What the harness must do because of a change of [`Phase`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Nothing.
    None,
    /// Asks Turns to stop at the end of their Step.
    Drain,
    /// Asks Turns to stop at the end of their Step, and starts building.
    DrainAndBuild,
    /// Lets Turns carry on, and stops any build.
    Abandon,
    /// Stops the harness.
    Exit(Exit),
}

/// Why the interactive harness stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// The person is finished.
    Quit,
    /// The Rebuild is ready: the harness should relaunch into this binary. The App can be run
    /// again if that fails.
    Rebuild(PathBuf),
}

impl Phase {
    /// Returns the phase after `trigger`, and what the harness must do about it. A trigger that
    /// means nothing in this phase changes nothing.
    #[must_use]
    pub fn after(self, trigger: Trigger) -> (Self, Effect) {
        match self {
            Self::Running => Self::after_running(&trigger),
            Self::DrainingToQuit => Self::after_draining_to_quit(&trigger),
            Self::DrainingToRebuild { binary } => Self::after_draining_to_rebuild(binary, trigger),
        }
    }

    /// Returns what a running harness does on `trigger`.
    fn after_running(trigger: &Trigger) -> (Self, Effect) {
        match trigger {
            Trigger::RebuildAsked => (
                Self::DrainingToRebuild { binary: None },
                Effect::DrainAndBuild,
            ),
            Trigger::QuitAsked => (Self::DrainingToQuit, Effect::Drain),
            _ => (Self::Running, Effect::None),
        }
    }

    /// Returns what a harness on its way to quitting does on `trigger`. Asking to quit a second
    /// time means not waiting.
    fn after_draining_to_quit(trigger: &Trigger) -> (Self, Effect) {
        match trigger {
            Trigger::Settled | Trigger::QuitAsked => (Self::Running, Effect::Exit(Exit::Quit)),
            Trigger::Interrupted => (Self::Running, Effect::Abandon),
            _ => (Self::DrainingToQuit, Effect::None),
        }
    }

    /// Returns what a harness on its way to relaunching into `binary` does on `trigger`.
    fn after_draining_to_rebuild(binary: Option<PathBuf>, trigger: Trigger) -> (Self, Effect) {
        match (trigger, binary) {
            (Trigger::Settled, Some(binary)) => {
                (Self::Running, Effect::Exit(Exit::Rebuild(binary)))
            }
            (Trigger::BuildFinished(None) | Trigger::Interrupted, _) => {
                (Self::Running, Effect::Abandon)
            }
            (Trigger::BuildFinished(built), _) => {
                (Self::DrainingToRebuild { binary: built }, Effect::None)
            }
            (_, binary) => (Self::DrainingToRebuild { binary }, Effect::None),
        }
    }

    /// Returns whether the harness is on its way out, so that nothing new should start.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        *self != Self::Running
    }

    /// Returns whether the harness is on its way to relaunching.
    #[must_use]
    pub fn is_rebuilding(&self) -> bool {
        matches!(self, Self::DrainingToRebuild { .. })
    }

    /// Returns what the harness is doing, with `waiting_for` Turns still running, if the person
    /// should know.
    #[must_use]
    pub fn progress(&self, waiting_for: usize) -> Option<String> {
        let doing = match self {
            Self::Running => return None,
            Self::DrainingToQuit => "quitting",
            Self::DrainingToRebuild { binary: None } => "building",
            Self::DrainingToRebuild { binary: Some(_) } => "built",
        };
        Some(format!("{doing}, {waiting_for} running, Alt-C stops"))
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
    use super::SessionEvent::{
        ApprovalAnswered, ApprovalRequested, Continued, TurnEnded, TurnStarted,
    };
    use super::SessionState::{AwaitingApproval, Idle, Parked, Working};
    use super::*;

    #[test]
    fn sessions_follow_their_transitions() {
        let transitions = [
            (Idle, TurnStarted, Working),
            (Working, ApprovalRequested, AwaitingApproval),
            (Working, TurnEnded, Idle),
            (AwaitingApproval, ApprovalAnswered, Working),
            (AwaitingApproval, TurnEnded, Idle),
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
            (Idle, ApprovalRequested),
            (Idle, ApprovalAnswered),
            (Idle, TurnEnded),
            (Working, TurnStarted),
            (Working, ApprovalAnswered),
            (AwaitingApproval, TurnStarted),
            (AwaitingApproval, ApprovalRequested),
            (Idle, SessionEvent::Parked),
            (AwaitingApproval, SessionEvent::Parked),
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
        assert!(AwaitingApproval.in_turn());
        assert!(!Parked.in_turn());
        assert!(Idle.is_waiting());
        assert!(!Working.is_waiting());
        assert!(AwaitingApproval.is_waiting());
        assert!(!Parked.is_waiting());
    }

    #[test]
    fn ctrl_c_only_ever_affects_the_session_on_screen() {
        assert_eq!(interrupt(Working, false), Interrupt::CancelTurn);
        assert_eq!(interrupt(Working, true), Interrupt::CancelTurn);
        assert_eq!(interrupt(AwaitingApproval, true), Interrupt::CancelTurn);
        assert_eq!(interrupt(Idle, false), Interrupt::Quit);
        assert_eq!(interrupt(Idle, true), Interrupt::RefuseToQuit);
    }

    #[test]
    fn the_harness_follows_its_transitions() {
        let binary = PathBuf::from("/bin/sstack");
        let rebuilding = || Phase::DrainingToRebuild { binary: None };
        let built = || Phase::DrainingToRebuild {
            binary: Some(binary.clone()),
        };
        let exit = |exit| Effect::Exit(exit);
        let transitions = [
            (
                Phase::Running,
                Trigger::RebuildAsked,
                rebuilding(),
                Effect::DrainAndBuild,
            ),
            (
                Phase::Running,
                Trigger::QuitAsked,
                Phase::DrainingToQuit,
                Effect::Drain,
            ),
            (
                Phase::DrainingToQuit,
                Trigger::Settled,
                Phase::Running,
                exit(Exit::Quit),
            ),
            (
                Phase::DrainingToQuit,
                Trigger::QuitAsked,
                Phase::Running,
                exit(Exit::Quit),
            ),
            (
                Phase::DrainingToQuit,
                Trigger::Interrupted,
                Phase::Running,
                Effect::Abandon,
            ),
            (
                rebuilding(),
                Trigger::BuildFinished(Some(binary.clone())),
                built(),
                Effect::None,
            ),
            (
                rebuilding(),
                Trigger::BuildFinished(None),
                Phase::Running,
                Effect::Abandon,
            ),
            (
                rebuilding(),
                Trigger::Interrupted,
                Phase::Running,
                Effect::Abandon,
            ),
            (
                built(),
                Trigger::Interrupted,
                Phase::Running,
                Effect::Abandon,
            ),
            (
                built(),
                Trigger::Settled,
                Phase::Running,
                exit(Exit::Rebuild(binary.clone())),
            ),
        ];
        for (from, trigger, to, effect) in transitions {
            let context = format!("{from:?} on {trigger:?}");
            assert_eq!(from.after(trigger), (to, effect), "{context}");
        }
    }

    #[test]
    fn triggers_that_mean_nothing_to_the_harness_change_nothing() {
        let rebuilding = Phase::DrainingToRebuild { binary: None };
        let ignored = [
            (Phase::Running, Trigger::Settled),
            (Phase::Running, Trigger::Interrupted),
            (Phase::Running, Trigger::BuildFinished(None)),
            (Phase::DrainingToQuit, Trigger::RebuildAsked),
            (rebuilding.clone(), Trigger::Settled),
            (rebuilding.clone(), Trigger::RebuildAsked),
            (rebuilding, Trigger::QuitAsked),
        ];
        for (phase, trigger) in ignored {
            let context = format!("{phase:?} on {trigger:?}");
            assert_eq!(
                phase.clone().after(trigger),
                (phase, Effect::None),
                "{context}"
            );
        }
    }

    #[test]
    fn the_person_is_told_what_a_drain_is_waiting_on() {
        assert_eq!(Phase::Running.progress(2), None);
        let quitting = Phase::DrainingToQuit.progress(2).unwrap();
        assert_eq!(quitting, "quitting, 2 running, Alt-C stops");
        let building = Phase::DrainingToRebuild { binary: None }
            .progress(1)
            .unwrap();
        assert!(building.starts_with("building, 1 running"), "{building}");
        let binary = Some(PathBuf::from("/b"));
        let built = Phase::DrainingToRebuild { binary }.progress(0).unwrap();
        assert!(built.starts_with("built, 0 running"), "{built}");
        assert!(!Phase::Running.is_draining());
        assert!(Phase::DrainingToQuit.is_draining());
        assert!(!Phase::DrainingToQuit.is_rebuilding());
    }
}
