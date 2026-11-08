//! Everything the window keeps for one Session, whether or not it is on screen.

use std::collections::VecDeque;
use std::mem;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use iced::Subscription;
use iced::widget::text_editor;
use sstack::agent::TurnEnd;
use sstack::queue::Waits;
use sstack::running_harness::SessionState;
use sstack::session::{self, Entry};
use sstack::session_controller::SessionController;
use sstack::session_report::{ApprovalRequest, SessionReport};
use sstack::turn_runner::AgentHandle;
use sstack::workspace::{self, SessionSummary};

use super::sidebar::{Activity, Row};
use super::{Message, Opened};
use crate::reports::Reports;
use crate::transcript::Transcript;

/// The name of a Session the person has neither named nor said anything in yet.
const UNTITLED: &str = "new session";

/// Something that ended while the person was looking at another Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unseen {
    /// A Turn ended, with output to read.
    Finished,
    /// A Turn stopped with an error.
    Failed,
}

/// One Session as the window keeps it, up to date while another is on screen, so that switching
/// to it finds its transcript, half-typed prompt and open questions as they were.
pub struct SessionView {
    /// The Session's id, which `--resume` accepts.
    id: String,
    /// Where the Session is stored.
    path: PathBuf,
    /// The first thing the person said in the Session, which is its Session Name until they give
    /// it one.
    first_prompt: Option<String>,
    /// The name the person gave the Session, if they gave it one.
    name: Option<String>,
    /// Control of the Session's Turns. Missing until the Session is first shown, because a
    /// Session the person never opens needs no agent.
    controller: Option<SessionController>,
    /// The Session's reports, on their way to the window. Missing until the controller is.
    reports: Option<Reports>,
    /// What the Session is doing until it has a controller, which then keeps track instead:
    /// parked if an earlier run left a Turn parked in it, and otherwise idle.
    stored: SessionState,
    /// How long the Session has waited for the person or worked, which orders the sidebar.
    waits: Waits,
    /// What the person has not seen yet.
    unseen: Option<Unseen>,
    /// What the person has typed and not yet sent.
    pub(super) prompt: text_editor::Content,
    /// What has been said in the Session, including before the window opened.
    pub(super) transcript: Transcript,
    /// Questions about shell commands that the Session's Turn is waiting on, oldest first. Only
    /// the oldest is shown. Dropping one unanswered, as closing the window does, denies it.
    questions: VecDeque<ApprovalRequest>,
    /// Whether the transcript keeps its newest output in view as more arrives. It stops once the
    /// person scrolls back, and starts again when they scroll to the end, start a Turn or come
    /// back to the Session.
    following: bool,
}

impl SessionView {
    /// Creates the view of a Session on disk, with no agent yet.
    pub fn stored(summary: SessionSummary) -> Self {
        let mut view = Self::blank(summary.path, summary.modified);
        view.first_prompt = summary.first_prompt;
        view.name = summary.name;
        if summary.parked {
            view.stored = SessionState::Parked;
        }
        view
    }

    /// Creates the view of a Session that has only just opened.
    pub fn opened(opened: Opened) -> Self {
        let mut view = Self::blank(opened.run.session.clone(), SystemTime::now());
        view.attach(opened);
        view
    }

    /// Creates a view of the Session at `path`, waiting for the person since `waiting_since`.
    fn blank(path: PathBuf, waiting_since: SystemTime) -> Self {
        Self {
            id: workspace::session_id(&path),
            path,
            first_prompt: None,
            name: None,
            controller: None,
            reports: None,
            stored: SessionState::Idle,
            waits: Waits::since(waiting_since),
            unseen: None,
            prompt: text_editor::Content::new(),
            transcript: Transcript::default(),
            questions: VecDeque::new(),
            following: true,
        }
    }

    /// Gives the Session the agent it was missing, telling the person how it was set up. A Turn
    /// an earlier run left parked stays parked until [`Self::continue_parked`].
    ///
    /// Must be called inside a Tokio runtime, which runs the Session's Turns.
    pub fn attach(&mut self, opened: Opened) {
        let Opened {
            run,
            reports,
            sender,
        } = opened;
        let parked = session::is_parked(&run.session).unwrap_or(false);
        let agent = AgentHandle::spawn(run.agent, sender);
        self.controller = Some(SessionController::new(agent, run.skills, parked));
        self.reports = Some(reports);
        let notices = run
            .notices
            .into_iter()
            .chain(listing_shortened(run.descriptions_left_out));
        notices.for_each(|notice| self.transcript.notice(notice));
    }

    /// Returns the Session's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns where the Session is stored.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns what the Session is doing.
    pub fn state(&self) -> SessionState {
        self.controller
            .as_ref()
            .map_or(self.stored, SessionController::state)
    }

    /// Returns whether the Session can run Turns yet.
    pub fn has_agent(&self) -> bool {
        self.controller.is_some()
    }

    /// Returns the Session's reports, once it has an agent to send them.
    #[cfg(test)]
    pub fn reports(&self) -> Option<&Reports> {
        self.reports.as_ref()
    }

    /// Returns where the Session's reports reach the window, tagged with its id. A Session with
    /// no agent yet reports nothing.
    pub fn subscription(&self) -> Subscription<Message> {
        let Some(reports) = &self.reports else {
            return Subscription::none();
        };
        reports
            .subscription()
            .with(self.id.clone())
            .map(|(id, report)| Message::Report(id, report))
    }

    /// Runs `act` on the Session's controller, if it has one yet, timing the waits that order
    /// the sidebar by what `act` changed.
    pub fn control<R>(&mut self, act: impl FnOnce(&mut SessionController) -> R) -> Option<R> {
        let controller = self.controller.as_mut()?;
        let before = controller.state();
        let result = act(controller);
        self.waits.moved(before, controller.state());
        Some(result)
    }

    /// Carries on the Turn that parked, if the Session has one and an agent to run it.
    pub fn continue_parked(&mut self) {
        self.control(SessionController::continue_parked);
    }

    /// Returns a value that sorts this Session into its place in the sidebar.
    pub fn queue_position(&self) -> (bool, SystemTime) {
        self.waits.position(self.state())
    }

    /// Returns how the sidebar shows this Session as of `now`.
    pub fn row(&self, now: SystemTime) -> Row {
        let name = self.name.as_ref().or(self.first_prompt.as_ref());
        Row {
            id: self.id.clone(),
            name: name.map_or_else(|| UNTITLED.to_owned(), String::clone),
            activity: self.activity(),
            waiting: self.state().is_waiting().then(|| self.waits.waited(now)),
        }
    }

    /// Returns what to tell the person about this Session at a glance.
    fn activity(&self) -> Activity {
        match (self.state(), self.unseen) {
            (SessionState::Working, _) => Activity::Working,
            (SessionState::AwaitingApproval, _) => Activity::Blocked,
            (SessionState::Idle, Some(Unseen::Failed)) => Activity::Failed,
            (SessionState::Idle, Some(Unseen::Finished)) => Activity::Done,
            (SessionState::Idle, None) => Activity::Idle,
            (SessionState::Parked, _) => Activity::Parked,
        }
    }

    /// Notes that the person is now looking at this Session, which brings its newest output back
    /// into view.
    pub fn viewed(&mut self) {
        self.unseen = None;
        self.following = true;
    }

    /// Lists the Session under `name`, or under its first prompt again if `name` is `None`. Only
    /// changes the window, so the caller stores the name.
    pub fn rename(&mut self, name: Option<&str>) {
        self.name = name.map(str::to_owned);
    }

    /// Shows what core reported about the Session, returning whether the transcript changed.
    /// `on_screen` says whether the person can see it. What it means for the Session's Turns is
    /// for the Running Harness to work out.
    pub fn observe(&mut self, report: &SessionReport, on_screen: bool) -> bool {
        match report {
            SessionReport::Approval(question) => self.questions.push_back(question.clone()),
            SessionReport::Resumed(entries) => {
                self.first_prompt = self.first_prompt.take().or(first_prompt(entries));
            }
            SessionReport::TurnEnded(Ok(TurnEnd::Paused)) => {}
            SessionReport::TurnEnded(outcome) => {
                self.questions.clear();
                self.unseen = (!on_screen).then_some(if outcome.is_ok() {
                    Unseen::Finished
                } else {
                    Unseen::Failed
                });
            }
            _ => {}
        }
        self.transcript.report(report)
    }

    /// Returns the oldest open question about a shell command, which is the one shown.
    pub fn question(&self) -> Option<&ApprovalRequest> {
        self.questions.front()
    }

    /// Answers the oldest open question about a shell command, if there is one: `allowed` says
    /// whether the command may run.
    pub fn answer(&mut self, allowed: bool) {
        if let Some(question) = self.questions.pop_front() {
            question.reply.send(allowed);
            self.control(SessionController::answered);
        }
    }

    /// Changes the prompt as the person asked.
    pub fn edit(&mut self, action: text_editor::Action) {
        self.prompt.perform(action);
    }

    /// Takes what the person typed out of the prompt, leaving it empty.
    pub fn take_prompt(&mut self) -> text_editor::Content {
        mem::take(&mut self.prompt)
    }

    /// Puts back what the person typed, after what they sent was refused.
    pub fn restore_prompt(&mut self, draft: text_editor::Content) {
        self.prompt = draft;
    }

    /// Notes that a Turn started on `text`: the transcript shows it, and the newest output comes
    /// back into view. The person's first input also names the Session, unless they have named
    /// it themselves.
    pub fn turn_started(&mut self, text: &str) {
        self.transcript.prompt(text.to_owned());
        self.first_prompt.get_or_insert_with(|| text.to_owned());
        self.following = true;
    }

    /// Notes whether the person scrolled the transcript to its newest output.
    pub fn scrolled(&mut self, at_end: bool) {
        self.following = at_end;
    }

    /// Returns whether the transcript keeps its newest output in view.
    pub fn following(&self) -> bool {
        self.following
    }

    /// Tells the person `words` in the transcript.
    pub fn notice(&mut self, words: String) {
        self.transcript.notice(words);
    }
}

/// Returns the first thing the person said among `entries`.
fn first_prompt(entries: &[Entry]) -> Option<String> {
    entries.iter().find_map(|entry| match entry {
        Entry::User { text } => Some(text.clone()),
        _ => None,
    })
}

/// Returns the warning that `count` Skills are listed to the model without their descriptions,
/// if any are.
fn listing_shortened(count: usize) -> Option<String> {
    (count > 0).then(|| {
        format!(
            "{count} Skill descriptions were left out of the system prompt, so the model may not \
             think to load those Skills"
        )
    })
}
