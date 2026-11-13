//! Everything the window keeps for one Session, whether or not it is on screen.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use iced::Subscription;
use iced::widget::text_editor;
use sstack::agent::TurnEnd;
use sstack::connections::Connections;
use sstack::question::{Answer, Response};
use sstack::queue::Waits;
use sstack::running_harness::SessionState;
use sstack::session::{self, Entry, SessionSummary};
use sstack::session_controller::SessionController;
use sstack::session_report::{ApprovalRequest, QuestionRequest, SessionReport};
use sstack::tools::Verdict;
use sstack::turn_runner::AgentHandle;
use sstack::vim::VimEvent;

use super::input::Input;
use super::sidebar::{Activity, Row};
use super::{Message, Opened};
use crate::asked::Asking;
use crate::reports::Reports;
use crate::transcript::Transcript;

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
    /// The first thing the person said in the Session, which it is known by while it has no
    /// Session Name of its own.
    first_prompt: Option<String>,
    /// The Session Name the person gave the Session, or that was made from their first input.
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
    /// What the person has typed and not yet sent, and how they type it.
    pub(super) prompt: Input,
    /// What has been said in the Session, including before the window opened.
    pub(super) transcript: Transcript,
    /// Questions about shell commands that the Session's Turn is waiting on, oldest first, which
    /// the transcript shows and the person answers in that order. Dropping one unanswered, as
    /// closing the window does, denies it. Each goes when its Turn ends or parks, and one that a
    /// Rebuild put off is asked again when the Turn carries on.
    approvals: VecDeque<ApprovalRequest>,
    /// The model's Questions that the Session's Turn is waiting on, oldest first, which the
    /// transcript shows and the person answers in that order. Dropping one unanswered, as closing
    /// the window does, gives the model no answer.
    asked: VecDeque<QuestionRequest>,
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
            id: session::session_id(&path),
            path,
            first_prompt: None,
            name: None,
            controller: None,
            reports: None,
            stored: SessionState::Idle,
            waits: Waits::since(waiting_since),
            unseen: None,
            prompt: Input::default(),
            transcript: Transcript::default(),
            approvals: VecDeque::new(),
            asked: VecDeque::new(),
            following: true,
        }
    }

    /// Gives the Session the agent it was missing, telling the person how it was set up, and
    /// starts nvim if the person wants it, which spares a Session never shown from running one. A
    /// Turn an earlier run left parked stays parked until [`Self::continue_parked`].
    ///
    /// Must be called inside a Tokio runtime, which runs the Session's Turns and its nvim.
    pub fn attach(&mut self, opened: Opened) {
        let Opened {
            run,
            reports,
            sender,
        } = opened;
        if run.vim {
            self.prompt.start_vim();
        }
        let parked = session::is_parked(&run.session).unwrap_or(false);
        if let Ok(Some(since)) = session::put_off_since(&run.session) {
            self.waits.put_off(since);
        }
        let agent = AgentHandle::spawn(run.agent, sender.clone());
        let controller =
            SessionController::new(agent, run.skills, parked).with_connections(run.connections);
        self.controller = Some(match run.naming {
            Some(naming) => controller.with_naming(naming, sender),
            None => controller,
        });
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

    /// Returns the Connections the Session can switch between, with the one it runs on now.
    /// Missing until the Session is opened.
    pub fn connections(&self) -> Option<&Connections> {
        self.controller.as_ref()?.connections()
    }

    /// Moves the Session to the Connection called `name` from its next Turn on, and tells the
    /// person whether it did.
    pub fn switch_connection(&mut self, name: &str) {
        let switched = self.control(|controller| controller.switch_connection(name));
        match switched {
            Some(Ok(())) => self.notice(format!("runs on {name} from the next Turn")),
            Some(Err(error)) => self.notice(format!("not switched: {error}")),
            None => {}
        }
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

    /// Returns where the Session's reports, and those of its nvim if the person types into one,
    /// reach the window, tagged with its id. A Session with no agent yet reports nothing.
    pub fn subscription(&self) -> Subscription<Message> {
        let Some(reports) = &self.reports else {
            return Subscription::none();
        };
        let reports = reports
            .subscription()
            .with(self.id.clone())
            .map(|(id, report)| Message::Report(id, report));
        let Input::Vim(vim) = &self.prompt else {
            return reports;
        };
        let vim = vim
            .subscription()
            .with(self.id.clone())
            .map(|(id, event)| Message::Vim(id, event));
        Subscription::batch([reports, vim])
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

    /// Returns what the Session is known by, or nothing while it has no name to show yet.
    pub fn known_name(&self) -> Option<String> {
        let naming = self
            .controller
            .as_ref()
            .is_some_and(SessionController::is_naming);
        session::known_name(self.name.as_deref(), self.first_prompt.as_deref(), naming)
    }

    /// Returns how the sidebar shows this Session as of `now`.
    pub fn row(&self, now: SystemTime) -> Row {
        Row {
            id: self.id.clone(),
            name: session::listed_name(self.known_name().as_deref()),
            activity: self.activity(),
            waiting: self.state().is_waiting().then(|| self.waits.waited(now)),
        }
    }

    /// Returns what to tell the person about this Session at a glance.
    fn activity(&self) -> Activity {
        match (self.state(), self.unseen) {
            (SessionState::Working, _) => Activity::Working,
            (SessionState::AwaitingAnswer, _) => Activity::Blocked,
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
            SessionReport::Approval(question) => self.approvals.push_back(question.clone()),
            SessionReport::Question(request) => self.asked.push_back(request.clone()),
            SessionReport::Resumed(entries) => {
                self.first_prompt = self.first_prompt.take().or(first_prompt(entries));
            }
            SessionReport::Named(Some(name)) => {
                self.name.get_or_insert_with(|| name.clone());
            }
            SessionReport::TurnEnded(Ok(TurnEnd::Paused | TurnEnd::PutOff)) => {
                self.approvals.clear();
                self.asked.clear();
            }
            SessionReport::TurnEnded(outcome) => {
                self.approvals.clear();
                self.asked.clear();
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

    /// Returns whether the Session's Turn is waiting for the person to answer a question about a
    /// shell command or the model's Questions.
    pub fn asking(&self) -> bool {
        !self.approvals.is_empty() || !self.asked.is_empty()
    }

    /// Answers the oldest open question about a shell command, if there is one: `allowed` says
    /// whether the command may run.
    pub fn answer(&mut self, allowed: bool) {
        if let Some(question) = self.approvals.pop_front() {
            question.reply.send(if allowed {
                Verdict::Allow
            } else {
                Verdict::Deny
            });
            self.transcript.answered(allowed);
            self.control(SessionController::answered);
        }
    }

    /// Picks `row` of the Question at position `question` among the model's open Questions.
    pub fn choose(&mut self, question: usize, row: usize) {
        if let Some(asked) = self.transcript.open_questions() {
            asked.choose(question, row);
        }
    }

    /// Sets the person's own words for the Question at position `question` among the model's
    /// open Questions.
    pub fn write(&mut self, question: usize, words: String) {
        if let Some(asked) = self.transcript.open_questions() {
            asked.write(question, words);
        }
    }

    /// Sends the person's answers to the model's oldest open Questions, if every one of them has
    /// an answer.
    pub fn send_answers(&mut self) {
        let answers = self
            .transcript
            .open_questions()
            .and_then(|asked| asked.answers());
        if let Some(answers) = answers {
            self.settle(Some(answers));
        }
    }

    /// Tells the model that the person would rather answer its oldest open Questions in the
    /// conversation.
    pub fn decline(&mut self) {
        self.settle(None);
    }

    /// Gives the model's oldest open Questions `answers`, or none, and lets the Turn go on.
    fn settle(&mut self, answers: Option<Vec<Answer>>) {
        let Some(request) = self.asked.pop_front() else {
            return;
        };
        if let Some(asked) = self.transcript.open_questions() {
            asked.state = answers.clone().map_or(Asking::Declined, Asking::Answered);
        }
        request
            .reply
            .send(answers.map_or(Response::Declined, Response::Answered));
        self.control(SessionController::answered);
    }

    /// Changes the plain prompt as the person asked.
    pub fn edit(&mut self, action: text_editor::Action) {
        self.prompt.perform(action);
    }

    /// Takes what the person typed out of the plain prompt, leaving it empty. Nvim hands its text
    /// back later instead, so this asks it to and returns `None`.
    pub fn take_prompt(&mut self) -> Option<text_editor::Content> {
        self.prompt.take()
    }

    /// Puts back what the person typed, after what they sent was refused.
    pub fn restore_prompt(&mut self, typed: text_editor::Content) {
        self.prompt.restore(typed);
    }

    /// Puts `text`, Queued Prompts that were never sent, back into the prompt at its cursor.
    pub fn give_back(&mut self, text: &str) {
        self.prompt.give_back(text);
    }

    /// Switches between typing into a real nvim and the plain prompt, keeping what is typed.
    pub fn toggle_vim(&mut self) {
        self.prompt.toggle();
    }

    /// Shows what the Session's nvim reported, returning whether it was taken: a report from an
    /// nvim the person already switched away from is not. If nvim was lost, the person is told
    /// they are back to the plain prompt.
    pub fn vim_reported(&mut self, event: &VimEvent) -> bool {
        let taken = self.prompt.vim_reported(event);
        if let (true, VimEvent::Lost { failure, .. }) = (taken, event) {
            let words = format!("typing into the plain prompt: {failure}. /vim tries again");
            self.notice(words);
        }
        taken
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
