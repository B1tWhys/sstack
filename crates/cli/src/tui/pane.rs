//! Everything the harness keeps for one Session, whether or not it is on screen.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use sstack_core::session::Entry;
use sstack_core::workspace::{self, SessionSummary};

use super::component::{Component, KeyOutcome};
use super::components::{Activity, Approval, Prompt, Row, Status, Transcript};
use super::event::Event;
use super::lifecycle::{SessionEvent, SessionState};
use super::worker::AgentHandle;

/// Titles a Session the person has not said anything in yet.
const UNTITLED: &str = "new session";

/// A Session the harness has started an agent for.
pub struct Opened {
    /// Runs the Session's Turns.
    pub agent: AgentHandle,
    /// Where the Session is stored.
    pub path: PathBuf,
}

/// Something that ended while the person was looking at another Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unseen {
    /// A Turn ended, with output to read.
    Finished,
    /// A Turn stopped with an error.
    Failed,
}

/// Keeps one Session's screen up to date while it runs in the background, so that switching to it
/// finds everything as it should be, half-typed input and open questions included.
pub struct Pane {
    /// The Session's id, which `--resume` accepts.
    id: String,
    /// Where the Session is stored.
    path: PathBuf,
    /// The first thing the person said in the Session.
    title: Option<String>,
    /// Starts and cancels Turns. Missing until the Session is first shown, because a Session the
    /// person never opens needs no agent.
    agent: Option<AgentHandle>,
    /// What the Session is doing.
    state: SessionState,
    /// When the Session last started waiting for the person, which orders the queue.
    waiting_since: SystemTime,
    /// When the running Turn started.
    working_since: SystemTime,
    /// What the person has not seen yet.
    unseen: Option<Unseen>,
    /// Asks about shell commands, over everything else.
    approval: Approval,
    /// Where the person types.
    prompt: Prompt,
    /// The Session so far.
    transcript: Transcript,
    /// The Session id and what the agent is doing.
    status: Status,
}

impl Pane {
    /// Creates the pane for a Session on disk, with no agent yet.
    pub fn stored(summary: SessionSummary) -> Self {
        let mut pane = Self::blank(summary.path, summary.modified);
        pane.title = Some(summary.first_prompt);
        pane
    }

    /// Creates the pane for a Session that has only just opened.
    pub fn opened(opened: Opened) -> Self {
        let mut pane = Self::blank(opened.path, SystemTime::now());
        pane.agent = Some(opened.agent);
        pane
    }

    /// Creates a pane for the Session at `path`, waiting for the person since `waiting_since`.
    fn blank(path: PathBuf, waiting_since: SystemTime) -> Self {
        let id = workspace::session_id(&path);
        Self {
            status: Status::new(id.clone()),
            id,
            path,
            title: None,
            agent: None,
            state: SessionState::Idle,
            waiting_since,
            working_since: waiting_since,
            unseen: None,
            approval: Approval::default(),
            prompt: Prompt::default(),
            transcript: Transcript::default(),
        }
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
        self.state
    }

    /// Returns whether the Session can run Turns yet.
    pub fn has_agent(&self) -> bool {
        self.agent.is_some()
    }

    /// Gives the Session the agent it was missing.
    pub fn attach(&mut self, agent: AgentHandle) {
        self.agent = Some(agent);
    }

    /// Returns a value that sorts this Session into its place in the sidebar: waiting Sessions
    /// first and the longest wait first among them, then working Sessions, longest-running first.
    pub fn queue_position(&self) -> (bool, SystemTime) {
        if self.state.is_waiting() {
            (false, self.waiting_since)
        } else {
            (true, self.working_since)
        }
    }

    /// Notes that the person is now looking at this Session.
    pub fn viewed(&mut self) {
        self.unseen = None;
    }

    /// Returns how the sidebar should show this Session as of `now`.
    pub fn row(&self, now: SystemTime) -> Row {
        Row {
            id: self.id.clone(),
            title: self.title.clone().unwrap_or_else(|| UNTITLED.to_owned()),
            activity: self.activity(),
            waiting: self.state.is_waiting().then(|| self.waited(now)),
        }
    }

    /// Returns what to tell the person about this Session at a glance.
    fn activity(&self) -> Activity {
        match (self.state, self.unseen) {
            (SessionState::Working, _) => Activity::Working,
            (SessionState::AwaitingApproval, _) => Activity::Blocked,
            (SessionState::Idle, Some(Unseen::Failed)) => Activity::Failed,
            (SessionState::Idle, Some(Unseen::Finished)) => Activity::Done,
            (SessionState::Idle, None) => Activity::Idle,
        }
    }

    /// Returns how long the Session has been waiting as of `now`. A clock that went backwards
    /// counts as no time at all.
    fn waited(&self, now: SystemTime) -> Duration {
        now.duration_since(self.waiting_since).unwrap_or_default()
    }

    /// Reacts to something this Session did. `on_screen` says whether the person can see it.
    pub fn observe(&mut self, event: &Event, on_screen: bool) {
        match event {
            Event::Resumed(entries) => self.title = self.title.take().or(first_prompt(entries)),
            Event::Approval(_) => self.moved(SessionEvent::ApprovalRequested),
            Event::TurnEnded(outcome) => {
                self.moved(SessionEvent::TurnEnded);
                self.unseen = (!on_screen).then_some(if outcome.is_ok() {
                    Unseen::Finished
                } else {
                    Unseen::Failed
                });
            }
            _ => {}
        }
        self.update(event);
    }

    /// Keeps the Session's state machine current, timing the waits that order the queue.
    fn moved(&mut self, event: SessionEvent) {
        let next = self.state.after(event);
        if next == self.state {
            return;
        }
        if next.is_waiting() {
            self.waiting_since = SystemTime::now();
        } else if self.state == SessionState::Idle {
            self.working_since = SystemTime::now();
        }
        self.state = next;
    }

    /// Returns every component, topmost first, which is the order key presses are offered in.
    fn components(&mut self) -> [&mut dyn Component; 4] {
        [
            &mut self.approval,
            &mut self.prompt,
            &mut self.transcript,
            &mut self.status,
        ]
    }

    /// Tells every component about an event.
    pub fn update(&mut self, event: &Event) {
        for component in self.components() {
            component.update(event);
        }
    }

    /// Offers a key press to each component in turn, returning what the one that used it did.
    pub fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        self.components()
            .into_iter()
            .map(|component| component.key(key))
            .find(|outcome| *outcome != KeyOutcome::Ignored)
            .unwrap_or(KeyOutcome::Ignored)
    }

    /// Notes that the person answered about a shell command.
    pub fn answered(&mut self) {
        self.moved(SessionEvent::ApprovalAnswered);
    }

    /// Asks the agent to run a Turn on `text`. The person's first input also titles the Session.
    pub fn start_turn(&mut self, text: &str) {
        let Some(agent) = &mut self.agent else {
            return;
        };
        agent.prompt(text.to_owned());
        self.title.get_or_insert_with(|| text.to_owned());
        self.moved(SessionEvent::TurnStarted);
    }

    /// Cancels the running Turn, if there is one.
    pub fn cancel(&self) {
        if let Some(agent) = &self.agent {
            agent.cancel();
        }
    }

    /// Sets what the status line says about other Sessions.
    pub fn set_others(&mut self, others: Option<String>) {
        self.status.set_others(others);
    }

    /// Draws the Session into `area`, with any question for the person on top.
    ///
    /// The prompt grows with what is typed, up to half of `area`.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let prompt_height = self.prompt.height(area.width).min(area.height / 2).max(3);
        let [transcript, status, prompt] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(prompt_height),
        ])
        .areas(area);
        self.transcript.render(frame, transcript);
        self.status.render(frame, status);
        self.prompt.render(frame, prompt);
        self.approval.render(frame, area);
    }
}

/// Returns the first thing the person said among `entries`.
fn first_prompt(entries: &[Entry]) -> Option<String> {
    entries.iter().find_map(|entry| match entry {
        Entry::User { text } => Some(text.clone()),
        _ => None,
    })
}
