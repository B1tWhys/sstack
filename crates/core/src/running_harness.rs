//! The rules for what each Session is doing and what stopping means for the Running Harness,
//! shared by every interface.
//!
//! The rules themselves are pure functions of their inputs, so each can be tested without an
//! interface or an agent. [`RunningHarness`] applies them to the Sessions an interface has open:
//! the interface feeds in what happened and carries out the [`Outcome`] that comes back.

use std::mem;
use std::path::PathBuf;

use crate::human_command::{Command, parse_command};
use crate::rebuild::Checkout;
use crate::session_controller::{OpenSessions, SessionController};
use crate::session_report::SessionReport;

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
///        |                           |     |    turn started
///        |              answered     |     | asked
///        |                           |     |
///        |    turn ended     +----------------+
///        +------------------ | AwaitingAnswer |
///                            +----------------+
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing is running, and the Session is waiting for the person's next input.
    Idle,
    /// A Turn is running without needing the person.
    Working,
    /// A Turn cannot continue until the person answers a question: whether a shell command may
    /// run, or the model's Questions.
    AwaitingAnswer,
    /// A Turn stopped between Steps so that the Running Harness could stop, and will carry on.
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

    /// Returns whether a Turn is running, which a Rebuild or a quit has to wait for.
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

/// Whether the Running Harness is carrying on or on its way out.
///
/// Every Session's Turns stop at the end of their Step while draining, so that nothing is cut
/// short. Parked Turns carry on afterwards, in this process if the drain is abandoned, or in the
/// next one.
///
/// ```text
///                     /quit
///        +--------------------------------+
///        |                                v
///   +---------+   interrupted, or     +----------------+  all settled,
///   |         | <-------------------- | DrainingToQuit |  or /quit again
///   |         |                       +----------------+ ---------------> exit
///   | Running |
///   |         |   /rebuild            +-------------------+  all settled,
///   |         | --------------------> | DrainingToRebuild |  and built
///   |         | <-------------------- +-------------------+ -----------> relaunch
///   +---------+   interrupted, or build failed
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Phase {
    /// Sessions work as normal.
    #[default]
    Running,
    /// The person is quitting, once Turns reach the end of their Step.
    DrainingToQuit,
    /// The Harness Source is building, and the Running Harness relaunches once that has worked and Turns
    /// have reached the end of their Step.
    DrainingToRebuild {
        /// The new binary, once the build has finished.
        binary: Option<PathBuf>,
    },
}

/// Something that moves the Running Harness between [`Phase`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// The person asked for a Rebuild of this checkout.
    RebuildAsked(Checkout),
    /// The person asked to quit.
    QuitAsked,
    /// No Session has a Turn running.
    Settled,
    /// The build finished, with the new binary if it worked.
    BuildFinished(Option<PathBuf>),
    /// The person asked to stop what the Running Harness is doing.
    Interrupted,
}

/// What the Running Harness must do because of a change of [`Phase`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Nothing.
    None,
    /// A request for Turns to stop at the end of their Step.
    Drain,
    /// A request for Turns to stop at the end of their Step, and the start of a build of this
    /// checkout.
    DrainAndBuild(Checkout),
    /// A return to normal work, in which Turns carry on and any build stops.
    Abandon,
    /// The end of the Running Harness.
    Exit(Exit),
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

impl Phase {
    /// Returns the phase after `trigger`, and what the Running Harness must do about it. A trigger that
    /// means nothing in this phase changes nothing.
    #[must_use]
    pub fn after(self, trigger: Trigger) -> (Self, Effect) {
        match self {
            Self::Running => Self::after_running(&trigger),
            Self::DrainingToQuit => Self::after_draining_to_quit(&trigger),
            Self::DrainingToRebuild { binary } => Self::after_draining_to_rebuild(binary, trigger),
        }
    }

    /// Returns what a Running Harness working as normal does on `trigger`.
    fn after_running(trigger: &Trigger) -> (Self, Effect) {
        match trigger {
            Trigger::RebuildAsked(checkout) => (
                Self::DrainingToRebuild { binary: None },
                Effect::DrainAndBuild(*checkout),
            ),
            Trigger::QuitAsked => (Self::DrainingToQuit, Effect::Drain),
            _ => (Self::Running, Effect::None),
        }
    }

    /// Returns what a Running Harness on its way to quitting does on `trigger`. Asking to quit a second
    /// time means not waiting.
    fn after_draining_to_quit(trigger: &Trigger) -> (Self, Effect) {
        match trigger {
            Trigger::Settled | Trigger::QuitAsked => (Self::Running, Effect::Exit(Exit::Quit)),
            Trigger::Interrupted => (Self::Running, Effect::Abandon),
            _ => (Self::DrainingToQuit, Effect::None),
        }
    }

    /// Returns what a Running Harness on its way to relaunching into `binary` does on `trigger`.
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

    /// Returns whether the Running Harness is on its way out, so that nothing new should start.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        *self != Self::Running
    }

    /// Returns whether the Running Harness is on its way to relaunching.
    #[must_use]
    pub fn is_rebuilding(&self) -> bool {
        matches!(self, Self::DrainingToRebuild { .. })
    }
}

/// What the person's request to interrupt does right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    /// Cancellation of the Turn the person is looking at.
    CancelTurn,
    /// The end of the Running Harness.
    Quit,
    /// A refusal that only says why, because quitting would stop work the person cannot see.
    RefuseToQuit,
}

/// Decides what the person's request to interrupt does. It only ever affects the Session they are
/// looking at, whose state is `shown`, so that one stray press cannot stop work they cannot see.
/// `others_in_turn` says whether any other Session has a Turn running.
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

/// The Running Harness as an interface drives it, which keeps its [`Phase`] and applies every
/// rule that spans the open Sessions.
///
/// An interface keeps one for the whole process, however many Sessions it has open. What happened
/// in one Session goes through [`Self::in_session`], which names that Session and reaches the
/// rest through [`OpenSessions`]. What happened to the whole Running Harness reaches every open
/// Session the same way. Whatever only the interface can do comes back as an [`Outcome`].
#[derive(Debug, Default)]
pub struct RunningHarness {
    /// Whether the Running Harness is carrying on or on its way out.
    phase: Phase,
}

/// What the interface has to do after the [`RunningHarness`] has acted, or may want to know.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<'a> {
    /// A Turn started in the focused Session on this text. The interface can use it at once, for
    /// instance to name the Session, before the Turn itself reports.
    TurnStarted(String),
    /// Nothing happened, for this reason, which the interface tells the person in its own words.
    Refused(Refusal),
    /// A Human Command that only the interface can carry out, because it is about how the
    /// interface keeps its Sessions: [`Command::New`], [`Command::NewMain`],
    /// [`Command::Archive`], [`Command::Vim`] or [`Command::Rename`]. No other comes back.
    Command(Command<'a>),
    /// A request to start building this checkout of the Harness Source for a Rebuild, whose
    /// result the interface passes to [`RunningHarness::build_finished`]. A
    /// [`Checkout::Worktree`] means the worktree of the Session the person asked from.
    StartBuild(Checkout),
    /// A request to stop the build, if one is running.
    CancelBuild,
    /// The end of the Running Harness.
    Exit(Exit),
}

/// Why the [`RunningHarness`] did not do what the person asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A Turn is running in the Session, and it runs one Turn at a time.
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

impl RunningHarness {
    /// Returns whether the Running Harness is carrying on or on its way out.
    #[must_use]
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Returns the Running Harness ready to act on something that happened in the `focus`
    /// Session, with `others` as every other open Session.
    #[must_use]
    pub fn in_session<'a>(
        &'a mut self,
        focus: &'a mut SessionController,
        others: &'a mut dyn OpenSessions,
    ) -> InSession<'a> {
        InSession {
            harness: self,
            sessions: Every { focus, others },
        }
    }

    /// Acts on the end of the build that [`Outcome::StartBuild`] asked for, which produced
    /// `built` if it worked. The interface tells the person why it failed, if it did.
    #[must_use]
    pub fn build_finished(
        &mut self,
        built: Option<PathBuf>,
        sessions: &mut dyn OpenSessions,
    ) -> Option<Outcome<'static>> {
        self.advance(Trigger::BuildFinished(built), sessions)
            .or_else(|| self.settle(sessions))
    }

    /// Lets every Session carry on in this process after [`Exit::Rebuild`] could not relaunch.
    pub fn relaunch_failed(&mut self, sessions: &mut dyn OpenSessions) {
        self.phase = Phase::Running;
        resume(sessions);
    }

    /// Starts a Turn on the Skill that the slash-prefixed `line` names in the `focus` Session, or
    /// says why it cannot.
    fn run_skill(&self, line: &str, focus: &mut SessionController) -> Outcome<'static> {
        match focus.skill_prompt(line) {
            Ok(_) if self.phase.is_draining() => Outcome::Refused(Refusal::Draining),
            Ok(text) => focus.start_turn(text, line),
            Err(refusal) => Outcome::Refused(refusal),
        }
    }

    /// Moves to the next phase and does what that takes to `sessions`.
    fn advance(
        &mut self,
        trigger: Trigger,
        sessions: &mut dyn OpenSessions,
    ) -> Option<Outcome<'static>> {
        let (phase, effect) = mem::take(&mut self.phase).after(trigger);
        self.phase = phase;
        match effect {
            Effect::None => None,
            Effect::Drain => {
                set_paused(sessions, true);
                self.settle(sessions)
            }
            Effect::DrainAndBuild(checkout) => {
                set_paused(sessions, true);
                Some(Outcome::StartBuild(checkout))
            }
            Effect::Abandon => {
                resume(sessions);
                Some(Outcome::CancelBuild)
            }
            Effect::Exit(exit) => Some(Outcome::Exit(exit)),
        }
    }

    /// Moves on if the phase is waiting for no Session to have a Turn running, and none has.
    fn settle(&mut self, sessions: &mut dyn OpenSessions) -> Option<Outcome<'static>> {
        (self.phase.is_draining() && !any_in_turn(sessions))
            .then(|| self.advance(Trigger::Settled, sessions))
            .flatten()
    }
}

/// The [`RunningHarness`] about to act on something that happened in one Session, from
/// [`RunningHarness::in_session`].
pub struct InSession<'a> {
    /// The Running Harness that acts.
    harness: &'a mut RunningHarness,
    /// The Session it acts on, and every other open one.
    sessions: Every<'a>,
}

impl InSession<'_> {
    /// Acts on a line the person entered in the Session: a Human Command, a Skill to run, or a
    /// prompt that starts a Turn.
    #[must_use]
    pub fn submit(self, line: &str) -> Option<Outcome<'_>> {
        let command = parse_command(line);
        if self.harness.phase.is_draining() && command.changes_work() {
            return Some(Outcome::Refused(Refusal::Draining));
        }
        match command {
            Command::Prompt(text) => Some(self.sessions.focus.start_turn(text.to_owned(), text)),
            Command::Unknown(line) => Some(self.harness.run_skill(line, self.sessions.focus)),
            Command::Rebuild(checkout) => self.advance(Trigger::RebuildAsked(checkout)),
            Command::Quit => self.quit(),
            Command::Empty => None,
            command => Some(Outcome::Command(command)),
        }
    }

    /// Acts on what core reported about the Session.
    #[must_use]
    pub fn report(mut self, report: &SessionReport) -> Option<Outcome<'static>> {
        self.sessions.focus.report(report);
        if matches!(report, SessionReport::TurnEnded(_)) && !self.harness.phase.is_draining() {
            self.sessions.focus.continue_parked();
        }
        self.harness.settle(&mut self.sessions)
    }

    /// Acts on the person's request to interrupt while looking at the Session. During a drain
    /// it backs out of the drain and does nothing else.
    #[must_use]
    pub fn interrupt(self) -> Option<Outcome<'static>> {
        if self.harness.phase.is_draining() {
            return self.advance(Trigger::Interrupted);
        }
        let Every { focus, others } = self.sessions;
        match interrupt(focus.state(), any_in_turn(others)) {
            Interrupt::CancelTurn => {
                focus.cancel();
                None
            }
            Interrupt::Quit => Some(Outcome::Exit(Exit::Quit)),
            Interrupt::RefuseToQuit => Some(Outcome::Refused(Refusal::OthersWorking)),
        }
    }

    /// Starts to quit, or stops waiting if already quitting. A Rebuild has to be backed out of
    /// first, because it would otherwise end in a relaunch.
    fn quit(self) -> Option<Outcome<'static>> {
        if self.harness.phase.is_rebuilding() {
            return Some(Outcome::Refused(Refusal::Draining));
        }
        self.advance(Trigger::QuitAsked)
    }

    /// Moves the Running Harness to its next phase and does what that takes to every open
    /// Session.
    fn advance(mut self, trigger: Trigger) -> Option<Outcome<'static>> {
        self.harness.advance(trigger, &mut self.sessions)
    }
}

/// The Session the person acted in together with every other open one.
struct Every<'a> {
    /// The Session the person acted in, or that core reported on.
    focus: &'a mut SessionController,
    /// Every other open Session.
    others: &'a mut dyn OpenSessions,
}

impl OpenSessions for Every<'_> {
    fn visit(&mut self, visit: &mut dyn FnMut(&mut SessionController)) {
        visit(self.focus);
        self.others.visit(visit);
    }
}

/// Returns whether any of `sessions` has a Turn running.
fn any_in_turn(sessions: &mut dyn OpenSessions) -> bool {
    let mut found = false;
    sessions.visit(&mut |session| found |= session.state().in_turn());
    found
}

/// Asks every Turn in `sessions` to stop at the end of its Step, or stops asking.
fn set_paused(sessions: &mut dyn OpenSessions, paused: bool) {
    sessions.visit(&mut |session| session.set_paused(paused));
}

/// Lets every one of `sessions` carry on, including any Turn that has already parked.
fn resume(sessions: &mut dyn OpenSessions) {
    sessions.visit(&mut |session| {
        session.set_paused(false);
        session.continue_parked();
    });
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
            (AwaitingAnswer, SessionEvent::Parked),
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

    #[test]
    fn ctrl_c_only_ever_affects_the_session_on_screen() {
        assert_eq!(interrupt(Working, false), Interrupt::CancelTurn);
        assert_eq!(interrupt(Working, true), Interrupt::CancelTurn);
        assert_eq!(interrupt(AwaitingAnswer, true), Interrupt::CancelTurn);
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
                Trigger::RebuildAsked(Checkout::Worktree),
                rebuilding(),
                Effect::DrainAndBuild(Checkout::Worktree),
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
            (Phase::DrainingToQuit, Trigger::RebuildAsked(Checkout::Main)),
            (rebuilding.clone(), Trigger::Settled),
            (rebuilding.clone(), Trigger::RebuildAsked(Checkout::Main)),
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
    fn only_a_drain_stops_new_work() {
        assert!(!Phase::Running.is_draining());
        assert!(Phase::DrainingToQuit.is_draining());
        assert!(!Phase::DrainingToQuit.is_rebuilding());
    }
}
