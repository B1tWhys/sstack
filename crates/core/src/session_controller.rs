//! What an interface keeps for each Session it has open, so that every interface follows the same
//! rules for what the person's input does to it.
//!
//! An interface keeps one [`SessionController`] per open Session and one
//! [`RunningHarness`](crate::running_harness::RunningHarness) for the whole process. It tells the
//! Running Harness what the person did and what core reported, naming the Session that happened in,
//! and carries out the [`Outcome`] that comes back. The Running Harness reaches the other open
//! Sessions through [`OpenSessions`], which the interface implements over however it keeps them.

use crate::agent::{AgentHandle, TurnEnd};
use crate::connection::Connections;
use crate::question::Response;
use crate::running_harness::{Outcome, Refusal, SessionEvent, SessionState};
use crate::session::Naming;
use crate::session_report::{Reply, ReportSender, SessionReport};
use crate::session_setup::SetupError;
use crate::skills::{self, Lookup, Skill};
use crate::tools::Verdict;

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
    /// What the running Turn waits for the person to answer, if anything, so that a Rebuild can
    /// put it off.
    open: Option<Open>,
    /// The Connections the Session can switch between. Missing for a Session that can never
    /// leave the one it was opened on.
    connections: Option<Connections>,
}

/// What a Session's running Turn waits for the person to answer, and where the answer goes.
enum Open {
    /// Whether a shell command may run.
    Approval(Reply<Verdict>),
    /// The model's Questions.
    Questions(Reply<Response>),
}

impl Open {
    /// Puts off what the Turn asked, so that it parks and asks again when it carries on.
    fn defer(self) {
        match self {
            Self::Approval(reply) => reply.send(Verdict::Defer),
            Self::Questions(reply) => reply.send(Response::Deferred),
        }
    }
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
            open: None,
            connections: None,
        }
    }

    /// Lets the person switch the Session between `connections`.
    #[must_use]
    pub fn with_connections(mut self, connections: Connections) -> Self {
        self.connections = Some(connections);
        self
    }

    /// Returns the Connections the Session can switch between, with the one it runs on now.
    /// Missing for a Session that can never leave the one it was opened on.
    #[must_use]
    pub fn connections(&self) -> Option<&Connections> {
        self.connections.as_ref()
    }

    /// Moves the Session to the Connection called `name`, from the next Turn on. A Turn that is
    /// running finishes on the Connection it started on. The Session keeps the choice, also
    /// after it is next opened, and the model that names it moves too if it has no name yet.
    ///
    /// # Errors
    /// Returns an error, and leaves the Session on its Connection, if it cannot switch, if
    /// settings have no such Connection, if its secret is in none of the places it is looked
    /// for, or if the choice cannot be kept.
    pub fn switch_connection(&mut self, name: &str) -> Result<(), SetupError> {
        let Some(connections) = &mut self.connections else {
            return Err(crate::connection::ConfigError::UnknownConnection(name.to_owned()).into());
        };
        let Some(switch) = connections.switch(name)? else {
            return Ok(());
        };
        if let NameWork::Wanted(naming, reports) =
            std::mem::replace(&mut self.naming, NameWork::Unwanted)
        {
            let naming = naming.served_by(switch.namer, switch.name_model);
            self.naming = NameWork::Wanted(naming, reports);
        }
        self.agent.switch(switch.service);
        Ok(())
    }

    /// Has `naming` give the Session its Session Name from the first Turn the person starts,
    /// reporting [`SessionReport::Named`] through `reports` once that is done.
    #[must_use]
    pub fn with_naming(mut self, naming: Naming, reports: ReportSender) -> Self {
        self.naming = NameWork::Wanted(naming, reports);
        self
    }

    /// Returns whether the Session's Session Name is being made, until when an interface shows
    /// [`UNTITLED`](crate::session::UNTITLED) rather than the first prompt.
    #[must_use]
    pub fn is_naming(&self) -> bool {
        matches!(self.naming, NameWork::Running)
    }

    /// Returns what the Session is doing.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Notes that the person answered what the Session's Turn asked: whether a shell command may
    /// run, or its Questions.
    pub fn answered(&mut self) {
        self.open = None;
        self.moved(SessionEvent::Answered);
    }

    /// Puts off what the running Turn waits for the person to answer, if anything, so that the
    /// Turn parks and asks the same thing again when it carries on. The person's own answer, if
    /// it comes later, does nothing.
    pub(crate) fn defer_open(&mut self) {
        if let Some(open) = self.open.take() {
            open.defer();
        }
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
            SessionReport::Approval(request) => {
                self.open = Some(Open::Approval(request.reply.clone()));
                SessionEvent::Asked
            }
            SessionReport::Question(request) => {
                self.open = Some(Open::Questions(request.reply.clone()));
                SessionEvent::Asked
            }
            SessionReport::TurnEnded(Ok(TurnEnd::Paused | TurnEnd::PutOff)) => SessionEvent::Parked,
            SessionReport::TurnEnded(_) => SessionEvent::TurnEnded,
            _ => return,
        };
        if matches!(report, SessionReport::TurnEnded(_)) {
            self.open = None;
        }
        self.moved(event);
    }

    /// Starts a Turn on `text`, or queues it for the running Turn if there is one. `typed` is the
    /// line the person typed for it, which differs from `text` when it runs a Skill, and which
    /// names the Session if this is its first Turn.
    pub(crate) fn start_turn(&mut self, text: String, typed: &str) -> Outcome<'static> {
        if self.state.in_turn() {
            self.agent.queue_prompt(text);
            return Outcome::Queued;
        }
        self.agent.prompt(text.clone());
        self.moved(SessionEvent::TurnStarted);
        self.start_naming(typed);
        Outcome::TurnStarted(text)
    }

    /// Starts a Turn on the prompts queued for a Turn that ended before it could take them, if
    /// there are any and no Turn is running.
    pub(crate) fn start_queued(&mut self) -> Option<Outcome<'static>> {
        if self.state.in_turn() {
            return None;
        }
        let text = self.agent.take_queued()?;
        Some(self.start_turn(text.clone(), &text))
    }

    /// Takes back the prompts queued for the running Turn that it has not taken yet, joined into
    /// one message, so that they are never sent. Returns `None` if there are none.
    pub(crate) fn take_queued(&self) -> Option<String> {
        self.agent.take_queued()
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
    use tokio::sync::oneshot;

    use super::*;
    use crate::agent::{Agent, AgentConfig};
    use crate::connection::scripted::ScriptedProvider;
    use crate::human_command::Command;
    use crate::rebuild::Checkout;
    use crate::running_harness::{Exit, RunningHarness};
    use crate::session::{AssistantEntry, Entry, SessionLog, StopReason, Usage};
    use crate::session_report::{ApprovalRequest, QuestionRequest};
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
        assert_eq!(submit("again"), Some(Outcome::Queued));
        assert_eq!(session.state(), SessionState::Working);
    }

    #[tokio::test]
    async fn prompts_queued_for_a_turn_that_ends_first_start_the_next_one_together() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();
        for line in ["go", "one", "two"] {
            let _ = harness
                .in_session(&mut session, &mut NoOtherSessions)
                .submit(line);
        }

        let next = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&ended(TurnEnd::Finished));

        assert_eq!(next, Some(Outcome::TurnStarted("one\n\ntwo".into())));
        assert_eq!(session.state(), SessionState::Working);
        let after = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&ended(TurnEnd::Finished));
        assert_eq!(after, None);
        assert_eq!(session.state(), SessionState::Idle);
    }

    #[tokio::test]
    async fn interrupting_a_turn_gives_back_what_was_queued_for_it() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();
        for line in ["go", "more"] {
            let _ = harness
                .in_session(&mut session, &mut NoOtherSessions)
                .submit(line);
        }

        let interrupted = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .interrupt();
        assert_eq!(interrupted, Some(Outcome::Withdrawn("more".into())));

        let cancelled = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&ended(TurnEnd::Cancelled));
        assert_eq!(cancelled, None);
        assert_eq!(session.state(), SessionState::Idle);
    }

    #[tokio::test]
    async fn a_turn_that_ends_during_a_drain_gives_back_what_was_queued_for_it() {
        let (dir, _) = git_project();
        let (mut working, mut idle) = (controller(&dir, "a"), controller(&dir, "b"));
        let mut harness = RunningHarness::default();
        for line in ["go", "more"] {
            let _ = harness
                .in_session(&mut working, &mut NoOtherSessions)
                .submit(line);
        }
        let _ = harness
            .in_session(&mut idle, &mut working)
            .submit("/rebuild");

        let ended = harness
            .in_session(&mut working, &mut idle)
            .report(&ended(TurnEnd::Finished));

        assert_eq!(ended, Some(Outcome::Withdrawn("more".into())));
        assert_eq!(working.state(), SessionState::Idle);
    }

    #[tokio::test]
    async fn a_session_switches_only_between_the_connections_it_was_given() {
        let (dir, _) = git_project();
        let mut fixed = controller(&dir, "a");
        assert!(fixed.connections().is_none());
        assert!(fixed.switch_connection("openai").is_err());

        let (naming, _) = naming(&dir, "b", "unused");
        let connections = crate::connection::test_support::built_in(dir.path().join("b.jsonl"));
        let mut session = controller(&dir, "b")
            .with_connections(connections)
            .with_naming(naming, ReportSender::new(|_| {}));

        session.switch_connection("anthropic").unwrap();
        session.switch_connection("openai").unwrap();
        assert!(session.switch_connection("nope").is_err());

        assert_eq!(session.connections().unwrap().current(), "openai");
        assert!(matches!(session.naming, NameWork::Wanted(..)));
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

    /// Returns a question about `touch ran`, as the Session's Turn would ask it, with the way to
    /// see how it was answered.
    fn question() -> (SessionReport, oneshot::Receiver<Verdict>) {
        let (reply, answer) = Reply::new();
        let request = ApprovalRequest {
            command: "touch ran".into(),
            note: None,
            reply,
        };
        (SessionReport::Approval(request), answer)
    }

    #[tokio::test]
    async fn a_rebuild_puts_off_an_open_question_rather_than_wait_for_it() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();
        let (asked, mut answer) = question();
        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("go");
        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&asked);

        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("/rebuild");

        assert_eq!(answer.try_recv(), Ok(Verdict::Defer));
        let parked = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&ended(TurnEnd::PutOff));
        assert_eq!(parked, None);
        assert_eq!(session.state(), SessionState::Parked);
        let binary = PathBuf::from("/bin/sstack");
        let built = harness.build_finished(Some(binary.clone()), &mut session);
        assert_eq!(built, Some(Outcome::Exit(Exit::Rebuild(binary))));
    }

    #[tokio::test]
    async fn a_rebuild_puts_off_the_models_questions_too() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();
        let (reply, mut response) = Reply::new();
        let asked = SessionReport::Question(QuestionRequest {
            questions: Vec::new(),
            reply,
        });
        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("go");
        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&asked);
        assert_eq!(session.state(), SessionState::AwaitingAnswer);

        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("/rebuild");

        assert_eq!(response.try_recv(), Ok(Response::Deferred));
    }

    #[tokio::test]
    async fn a_question_asked_during_a_rebuild_is_put_off_at_once() {
        let (dir, _) = git_project();
        let (mut working, mut idle) = (controller(&dir, "a"), controller(&dir, "b"));
        let mut harness = RunningHarness::default();
        let (asked, mut answer) = question();
        let _ = harness
            .in_session(&mut working, &mut NoOtherSessions)
            .submit("go");
        let _ = harness
            .in_session(&mut idle, &mut working)
            .submit("/rebuild");

        let _ = harness.in_session(&mut working, &mut idle).report(&asked);

        assert_eq!(answer.try_recv(), Ok(Verdict::Defer));
    }

    #[tokio::test]
    async fn quitting_still_waits_for_the_person_to_answer() {
        let (dir, _) = git_project();
        let mut session = controller(&dir, "a");
        let mut harness = RunningHarness::default();
        let (asked, mut answer) = question();
        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("go");
        let _ = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .report(&asked);

        let quit = harness
            .in_session(&mut session, &mut NoOtherSessions)
            .submit("/quit");

        assert_eq!(quit, None);
        assert_eq!(answer.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        assert_eq!(session.state(), SessionState::AwaitingAnswer);
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
