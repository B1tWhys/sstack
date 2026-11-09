//! What an interface keeps for each Session it has open, so that every interface follows the same
//! rules for what the person's input does to it.
//!
//! An interface keeps one [`SessionController`] per open Session and one
//! [`RunningHarness`](crate::running_harness::RunningHarness) for the whole process. It tells the
//! Running Harness what the person did and what core reported, naming the Session that happened in,
//! and carries out the [`Outcome`] that comes back. The Running Harness reaches the other open
//! Sessions through [`OpenSessions`], which the interface implements over however it keeps them.

use crate::agent::TurnEnd;
use crate::running_harness::{Outcome, Refusal, SessionEvent, SessionState};
use crate::session_name::Naming;
use crate::session_report::{ReportSender, SessionReport};
use crate::skills::{self, Lookup, Skill};
use crate::turn_runner::AgentHandle;

/// Control of one open Session, which starts, pauses and cancels its Turns and keeps track of what
/// it is doing, by the rules every interface shares.
///
/// Most of what changes it goes through the
/// [`RunningHarness`](crate::running_harness::RunningHarness), because whether a Turn may start
/// or must stop depends on every other open Session too.
pub struct SessionController {
    /// The agent that runs the Session's Turns.
    agent: AgentHandle,
    /// What the Session is doing.
    state: SessionState,
    /// The Skills the person can run by name in the Session.
    skills: Vec<Skill>,
    /// How far the Session is from having a Session Name made for it.
    naming: NameWork,
}

/// How far a Session is from having a Session Name made from the first thing the person says in
/// it.
///
/// ```text
///   +--------+  turn started  +---------+  named, or not  +----------+
///   | Wanted | -------------> | Running | --------------> | Unwanted |
///   +--------+                +---------+                 +----------+
/// ```
enum NameWork {
    /// Nothing will be made, because the Session needs no name or one has been made already.
    Unwanted,
    /// A name will be made from the next Turn the person starts, and reported to these reports.
    Wanted(Naming, ReportSender),
    /// A name is being made, and [`SessionReport::Named`] will say when it is done.
    Running,
}

impl SessionController {
    /// Takes control of the Session that `agent` runs, in which the person can run `skills` by
    /// name. `parked` says whether an earlier run left a Turn parked in it, which
    /// [`Self::continue_parked`] carries on.
    #[must_use]
    pub fn new(agent: AgentHandle, skills: Vec<Skill>, parked: bool) -> Self {
        let state = if parked {
            SessionState::Parked
        } else {
            SessionState::Idle
        };
        Self {
            agent,
            state,
            skills,
            naming: NameWork::Unwanted,
        }
    }

    /// Has `naming` give the Session its Session Name from the first Turn the person starts,
    /// reporting [`SessionReport::Named`] through `reports` once that is done.
    #[must_use]
    pub fn with_naming(mut self, naming: Naming, reports: ReportSender) -> Self {
        self.naming = NameWork::Wanted(naming, reports);
        self
    }

    /// Returns whether the Session's Session Name is being made, until when an interface shows
    /// [`UNTITLED`](crate::session_name::UNTITLED) rather than the first prompt.
    #[must_use]
    pub fn is_naming(&self) -> bool {
        matches!(self.naming, NameWork::Running)
    }

    /// Returns what the Session is doing.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Notes that the person answered the Session's question about a shell command.
    pub fn answered(&mut self) {
        self.moved(SessionEvent::ApprovalAnswered);
    }

    /// Carries on the Turn that parked, if there is one. An interface calls this once it has
    /// opened a Session an earlier run left parked.
    pub fn continue_parked(&mut self) {
        if self.state == SessionState::Parked {
            self.agent.continue_turn();
            self.moved(SessionEvent::Continued);
        }
    }

    /// Asks the Session's Turns to stop at the end of their Step, or stops asking. The Running
    /// Harness does this to every open Session as it drains and when it stops draining, so an
    /// interface has no need to.
    pub fn set_paused(&self, paused: bool) {
        self.agent.set_paused(paused);
    }

    /// Keeps up with what core reported about the Session.
    pub(crate) fn report(&mut self, report: &SessionReport) {
        let event = match report {
            SessionReport::Named(_) => {
                self.naming = NameWork::Unwanted;
                return;
            }
            SessionReport::Approval(_) => SessionEvent::ApprovalRequested,
            SessionReport::TurnEnded(Ok(TurnEnd::Paused)) => SessionEvent::Parked,
            SessionReport::TurnEnded(_) => SessionEvent::TurnEnded,
            _ => return,
        };
        self.moved(event);
    }

    /// Starts a Turn on `text`, unless one is already running. `typed` is the line the person
    /// typed for it, which differs from `text` when it runs a Skill, and which names the Session
    /// if this is its first Turn.
    pub(crate) fn start_turn(&mut self, text: String, typed: &str) -> Outcome<'static> {
        if self.state.in_turn() {
            return Outcome::Refused(Refusal::Busy);
        }
        self.agent.prompt(text.clone());
        self.moved(SessionEvent::TurnStarted);
        self.start_naming(typed);
        Outcome::TurnStarted(text)
    }

    /// Starts making the Session's Session Name from `typed`, if it wants one.
    fn start_naming(&mut self, typed: &str) {
        if let NameWork::Wanted(naming, reports) =
            std::mem::replace(&mut self.naming, NameWork::Unwanted)
        {
            naming.spawn(typed.to_owned(), reports);
            self.naming = NameWork::Running;
        }
    }

    /// Returns the prompt that runs the Skill the slash-prefixed `line` names, with the rest of
    /// the line added after its instructions, or why there is none.
    pub(crate) fn skill_prompt(&self, line: &str) -> Result<String, Refusal> {
        let request = line.trim_start_matches('/');
        let (name, added) = request
            .split_once(char::is_whitespace)
            .unwrap_or((request, ""));
        match skills::find(&self.skills, name) {
            Lookup::Found(skill) => {
                let loaded = skills::load(skill).map_err(|error| Refusal::UnreadableSkill {
                    name: name.to_owned(),
                    error: error.to_string(),
                })?;
                Ok(format!("{loaded}\n\n{}", added.trim())
                    .trim_end()
                    .to_owned())
            }
            Lookup::Ambiguous(names) => Err(Refusal::AmbiguousSkill {
                name: name.to_owned(),
                names: names.into_iter().map(str::to_owned).collect(),
            }),
            Lookup::Missing => Err(Refusal::UnknownCommand(line.to_owned())),
        }
    }

    /// Cancels the running Turn, if there is one.
    pub(crate) fn cancel(&self) {
        self.agent.cancel();
    }

    /// Keeps the Session's state machine current.
    fn moved(&mut self, event: SessionEvent) {
        self.state = self.state.after(event);
    }
}

/// Some of an interface's open Sessions, however it keeps them, which the Running Harness reaches
/// all at once.
///
/// A call that names the Session the person acted in takes every other open Session this way, and
/// a call about the whole Running Harness takes every open Session.
pub trait OpenSessions {
    /// Calls `visit` once on the controller of each of these Sessions.
    fn visit(&mut self, visit: &mut dyn FnMut(&mut SessionController));
}

impl OpenSessions for SessionController {
    fn visit(&mut self, visit: &mut dyn FnMut(&mut SessionController)) {
        visit(self);
    }
}

/// No Sessions at all, for an interface whose only open Session is the one the person acted in.
pub struct NoOtherSessions;

impl OpenSessions for NoOtherSessions {
    fn visit(&mut self, _visit: &mut dyn FnMut(&mut SessionController)) {}
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::*;
    use crate::agent::{Agent, AgentConfig};
    use crate::human_command::Command;
    use crate::provider::scripted::ScriptedProvider;
    use crate::rebuild::Checkout;
    use crate::running_harness::{Exit, RunningHarness};
    use crate::session::{AssistantEntry, Entry, SessionLog, StopReason, Usage};
    use crate::tools::project::Project;
    use crate::tools::test_support::{disabled, git_project};
    use crate::tools::{DenyAll, ToolBox};

    /// Opens a new Session called `name` in `dir`, whose reports go nowhere, so that what the
    /// controller knows of it comes only from what a test reports by hand.
    fn controller(dir: &TempDir, name: &str) -> SessionController {
        let project = Arc::new(Project::new(dir.path()).unwrap());
        let config = AgentConfig {
            provider: Arc::new(ScriptedProvider::new(vec![])),
            model: "scripted".into(),
            system: String::new(),
            tools: ToolBox::standard()
                .project(project)
                .sandbox(disabled())
                .approver(Arc::new(DenyAll))
                .call(),
            context_limit: 1000,
        };
        let log = SessionLog::create(&dir.path().join(name).with_extension("jsonl")).unwrap();
        let agent = Agent::new(config, log, vec![]).unwrap();
        let agent = AgentHandle::spawn(agent, ReportSender::new(|_| {}));
        SessionController::new(agent, Vec::new(), false)
    }

    /// Returns a way to name the Session called `name` in `dir`, with a model that replies
    /// `title`, along with that model.
    fn naming(dir: &TempDir, name: &str, title: &str) -> (Naming, Arc<ScriptedProvider>) {
        let reply = AssistantEntry {
            text: title.into(),
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
            payload: None,
        };
        let provider = Arc::new(ScriptedProvider::new([Ok(reply)]));
        let log = dir.path().join(name).with_extension("jsonl");
        (
            Naming::new(provider.clone(), "scripted".into(), log),
            provider,
        )
    }

    /// Returns the report of a Turn that ended for `end`.
    fn ended(end: TurnEnd) -> SessionReport {
        SessionReport::TurnEnded(Ok(end))
    }

    #[tokio::test]
    async fn a_prompt_starts_a_turn_and_the_next_waits_for_it_to_end() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();
        let mut submit = |line| {
            harness
                .in_session(&mut session, &mut NoOtherSessions)
                .submit(line)
        };

        let started = submit("hello");
        assert_eq!(started, Some(Outcome::TurnStarted("hello".into())));
        assert_eq!(submit("again"), Some(Outcome::Refused(Refusal::Busy)));
        assert_eq!(session.state(), SessionState::Working);
    }

    #[tokio::test]
    async fn the_first_turn_names_the_session_after_what_was_typed() {
        let (dir, _) = git_project();
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        let reports = ReportSender::new(move |report| {
            let _ = sender.send(report);
        });
        let (naming, provider) = naming(&dir, "a", "Say hello");
        let mut session = controller(&dir, "a").with_naming(naming, reports);
        let mut harness = RunningHarness::default();
        assert!(!session.is_naming());

        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("hello");
        assert!(session.is_naming());
        let named = received.recv().await.unwrap();
        assert!(matches!(&named, SessionReport::Named(Some(name)) if name == "Say hello"));
        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&named);

        assert!(!session.is_naming());
        let typed = Entry::User {
            text: "hello".into(),
        };
        assert_eq!(provider.requests(), [[typed]]);
    }

    #[tokio::test]
    async fn the_interface_carries_out_only_the_human_commands_about_its_sessions() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();
        let mut submit = |line| {
            harness
                .in_session(&mut session, &mut NoOtherSessions)
                .submit(line)
        };

        assert_eq!(submit("/new"), Some(Outcome::Command(Command::New)));
        let renamed = Some(Outcome::Command(Command::Rename(Some("x"))));
        assert_eq!(submit("/rename x"), renamed);
        assert_eq!(submit(""), None);
        let unknown = Refusal::UnknownCommand("/nope".into());
        assert_eq!(submit("/nope"), Some(Outcome::Refused(unknown)));
    }

    #[tokio::test]
    async fn quitting_waits_for_every_open_session_to_reach_the_end_of_its_step() {
        let (dir, _) = git_project();
        let (mut working, mut idle) = (controller(&dir, "a"), controller(&dir, "b"));
        let mut harness = RunningHarness::default();
        let _ = harness
            .in_session(&mut working, &mut NoOtherSessions)
            .submit("go");

        let quit = harness.in_session(&mut idle, &mut working).submit("/quit");
        assert_eq!(quit, None);
        let refused = harness.in_session(&mut idle, &mut working).submit("hi");
        assert_eq!(refused, Some(Outcome::Refused(Refusal::Draining)));

        let parked = ended(TurnEnd::Paused);
        let settled = harness.in_session(&mut working, &mut idle).report(&parked);
        assert_eq!(settled, Some(Outcome::Exit(Exit::Quit)));
        assert_eq!(working.state(), SessionState::Parked);
    }

    #[tokio::test]
    async fn a_rebuild_relaunches_once_built_with_nothing_running() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();

        let asked = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("/rebuild");
        assert_eq!(asked, Some(Outcome::StartBuild(Checkout::Main)));

        let binary = PathBuf::from("/bin/sstack");
        let built = harness.build_finished(Some(binary.clone()), &mut session);
        assert_eq!(built, Some(Outcome::Exit(Exit::Rebuild(binary))));
    }

    #[tokio::test]
    async fn a_rebuild_of_the_worktree_asks_for_that_checkout_to_be_built() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();

        let asked = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("/rebuild --worktree");

        assert_eq!(asked, Some(Outcome::StartBuild(Checkout::Worktree)));
        assert!(harness.phase().is_rebuilding());
    }

    #[tokio::test]
    async fn interrupting_a_drain_backs_out_and_carries_on_parked_turns() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();
        for line in ["go", "/rebuild"] {
            let _ = harness
                .in_session(&mut session, &mut NoOtherSessions)
                .submit(line);
        }
        let waiting = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&ended(TurnEnd::Paused));
        assert_eq!(waiting, None);

        let backed_out = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .interrupt();

        assert_eq!(backed_out, Some(Outcome::CancelBuild));
        assert!(!harness.phase().is_draining());
        assert_eq!(session.state(), SessionState::Working);
    }

    #[tokio::test]
    async fn interrupting_quits_only_once_no_other_session_is_working() {
        let (dir, _) = git_project();
        let (mut working, mut idle) = (controller(&dir, "a"), controller(&dir, "b"));
        let mut harness = RunningHarness::default();
        let _ = harness
            .in_session(&mut working, &mut NoOtherSessions)
            .submit("go");

        let refused = harness.in_session(&mut idle, &mut working).interrupt();
        assert_eq!(refused, Some(Outcome::Refused(Refusal::OthersWorking)));
        let cancelled = harness.in_session(&mut working, &mut idle).interrupt();
        assert_eq!(cancelled, None);

        let cancelled = ended(TurnEnd::Cancelled);
        let _ = harness
            .in_session(&mut working, &mut idle)
            .report(&cancelled);
        let quit = harness.in_session(&mut idle, &mut working).interrupt();
        assert_eq!(quit, Some(Outcome::Exit(Exit::Quit)));
    }
}
