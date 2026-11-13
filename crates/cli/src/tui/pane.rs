//! Everything the harness keeps for one Session, whether or not it is on screen.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossterm::cursor::SetCursorStyle;
use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use sstack::agent::{AgentHandle, TurnEnd};
use sstack::commands::Skill;
use sstack::connection::Connections;
use sstack::open_session::SessionController;
use sstack::queue::Waits;
use sstack::running_harness::SessionState;
use sstack::session::{self, Entry, Naming, SessionSummary};
use sstack::session_report::SessionReport;

use super::component::{Component, KeyOutcome};
use super::components::{
    Activity, Approval, ConnectionPicker, Input, Questions, Row, Status, Transcript,
};
use super::event::{Event, SessionSender};

/// A Session the harness has started an agent for.
pub struct Opened {
    /// The agent that runs the Session's Turns.
    pub agent: AgentHandle,
    /// Where the Session is stored.
    pub path: PathBuf,
    /// Things the person should know about how the Session was set up, in the order they
    /// happened.
    pub notices: Vec<String>,
    /// Where the Session's input reports.
    pub events: SessionSender,
    /// Whether the person wants to type into a real nvim.
    pub vim: bool,
    /// Every Skill installed for the Session, which the person can run by name.
    pub skills: Vec<Skill>,
    /// The way to give the Session its Session Name from the first thing the person says in it.
    /// Missing if it needs none.
    pub naming: Option<Naming>,
    /// The Connections the Session can switch between, with the one it runs on now.
    pub connections: Connections,
}

/// Something that ended while the person was looking at another Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unseen {
    /// A Turn ended, with output to read.
    Finished,
    /// A Turn stopped with an error.
    Failed,
}

/// One Session's screen, kept up to date while it runs in the background, so that switching to it
/// finds everything as it should be, half-typed input and open questions included.
pub struct Pane {
    /// The Session's id, which `--resume` accepts.
    id: String,
    /// Where the Session is stored.
    path: PathBuf,
    /// The first thing the person said in the Session, which it is known by while it has no
    /// Session Name of its own.
    first_prompt: Option<String>,
    /// The Session Name the person gave the Session, or that was made from their first input.
    name: Option<String>,
    /// Control of the Session's Turns. Missing until the Session is first shown, because
    /// a Session the person never opens needs no agent.
    controller: Option<SessionController>,
    /// Where the Session's input reports. Missing until the controller is.
    events: Option<SessionSender>,
    /// What the Session is doing until it has a controller, which then keeps track instead:
    /// parked if an earlier run left a Turn parked in it, and otherwise idle.
    stored: SessionState,
    /// How long the Session has waited for the person or worked, which orders the queue.
    waits: Waits,
    /// What the person has not seen yet.
    unseen: Option<Unseen>,
    /// The questions about shell commands, shown over everything else.
    approval: Approval,
    /// The Connections the person can switch the Session to, shown when they ask.
    picker: ConnectionPicker,
    /// The model's Questions, shown over everything else.
    questions: Questions,
    /// Where the person types.
    input: Input,
    /// The Session so far.
    transcript: Transcript,
    /// The Session id and what the agent is doing.
    status: Status,
}

impl Pane {
    /// Creates the pane for a Session on disk, with no agent yet.
    pub fn stored(summary: SessionSummary) -> Self {
        let mut pane = Self::blank(summary.path, summary.modified);
        pane.first_prompt = summary.first_prompt;
        pane.name = summary.name;
        if summary.parked {
            pane.stored = SessionState::Parked;
        }
        pane
    }

    /// Creates the pane for a Session that has only just opened.
    pub fn opened(opened: Opened) -> Self {
        let mut pane = Self::blank(opened.path.clone(), SystemTime::now());
        pane.attach(opened);
        pane
    }

    /// Creates a pane for the Session at `path`, waiting for the person since `waiting_since`.
    fn blank(path: PathBuf, waiting_since: SystemTime) -> Self {
        let id = session::session_id(&path);
        Self {
            status: Status::new(id.clone()),
            id,
            path,
            first_prompt: None,
            name: None,
            controller: None,
            events: None,
            stored: SessionState::Idle,
            waits: Waits::since(waiting_since),
            unseen: None,
            approval: Approval::default(),
            picker: ConnectionPicker::default(),
            questions: Questions::default(),
            input: Input::default(),
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

    /// Lists the Session under `name`, or under its first prompt again if `name` is `None`. Only
    /// changes the screen, so the caller stores the name.
    pub fn rename(&mut self, name: Option<&str>) {
        self.name = name.map(str::to_owned);
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

    /// Gives the Session the agent it was missing, and starts nvim if the person wants it, which
    /// spares a Session never opened from running one.
    pub fn attach(&mut self, opened: Opened) {
        let parked = self.stored == SessionState::Parked;
        if parked {
            self.keep_put_off_wait();
        }
        self.status.set_connection(opened.connections.current());
        let controller = SessionController::new(opened.agent, opened.skills, parked)
            .with_connections(opened.connections);
        self.controller = Some(match opened.naming {
            Some(naming) => controller.with_naming(naming, opened.events.reporter()),
            None => controller,
        });
        if opened.vim {
            self.input.start_vim(&opened.events);
        }
        self.events = Some(opened.events);
        for notice in opened.notices {
            self.update(&Event::Report(SessionReport::Notice(notice)));
        }
    }

    /// Keeps how long the Session has waited for the person, if its Turn parked because a
    /// Rebuild put off what it asked them, so that asking again does not send it down the
    /// sidebar.
    fn keep_put_off_wait(&mut self) {
        if let Ok(Some(since)) = session::put_off_since(&self.path) {
            self.waits.put_off(since);
        }
    }

    /// Returns a value that sorts this Session into its place in the sidebar: waiting Sessions
    /// first and the longest wait first among them, then working Sessions, longest-running first.
    pub fn queue_position(&self) -> (bool, SystemTime) {
        self.waits.position(self.state())
    }

    /// Notes that the person is now looking at this Session.
    pub fn viewed(&mut self) {
        self.unseen = None;
    }

    /// Returns what the Session is known by, or nothing while it has no name to show yet.
    pub fn known_name(&self) -> Option<String> {
        let naming = self
            .controller
            .as_ref()
            .is_some_and(SessionController::is_naming);
        session::known_name(self.name.as_deref(), self.first_prompt.as_deref(), naming)
    }

    /// Returns how the sidebar should show this Session as of `now`.
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

    /// Shows something this Session did. `on_screen` says whether the person can see it. What
    /// it means for the Session's Turns is for the Running Harness to work out.
    pub fn observe(&mut self, event: &Event, on_screen: bool) {
        match event {
            Event::Report(SessionReport::Resumed(entries)) => {
                self.first_prompt = self.first_prompt.take().or(first_prompt(entries));
            }
            Event::Report(SessionReport::Named(Some(name))) => {
                self.name.get_or_insert_with(|| name.clone());
            }
            Event::Report(SessionReport::TurnEnded(Ok(TurnEnd::Paused | TurnEnd::PutOff))) => {}
            Event::Report(SessionReport::TurnEnded(outcome)) => {
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

    /// Runs `act` on the Session's controller, if it has one yet, timing the waits that order
    /// the queue by what `act` changed.
    pub fn control<R>(&mut self, act: impl FnOnce(&mut SessionController) -> R) -> Option<R> {
        let controller = self.controller.as_mut()?;
        let before = controller.state();
        let result = act(controller);
        self.waits.moved(before, controller.state());
        Some(result)
    }

    /// Returns every component, topmost first, which is the order key presses are offered in.
    fn components(&mut self) -> [&mut dyn Component; 6] {
        [
            &mut self.approval,
            &mut self.questions,
            &mut self.picker,
            &mut self.input,
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

    /// Switches between typing into a real nvim and the plain prompt.
    pub fn toggle_vim(&mut self) {
        if let Some(events) = &self.events {
            self.input.toggle(events);
        }
    }

    /// Returns the cursor shape the Session's input calls for.
    pub fn cursor_style(&self) -> SetCursorStyle {
        self.input.cursor_style()
    }

    /// Notes that the person answered about a shell command.
    pub fn answered(&mut self) {
        self.control(SessionController::answered);
    }

    /// Notes that the person answered the model's Questions or declined to, recording `note` in
    /// the transcript.
    pub fn questions_answered(&mut self, note: String) {
        self.answered();
        self.update(&Event::Report(SessionReport::Notice(note)));
    }

    /// Notes that a Turn started on `text`. The person's first input also names the Session,
    /// unless they have named it themselves.
    pub fn turn_started(&mut self, text: &str) {
        self.first_prompt.get_or_insert_with(|| text.to_owned());
    }

    /// Puts `text`, Queued Prompts that were never sent, back into the Draft at the cursor, so
    /// that the person can change and send it again.
    pub fn give_back(&mut self, text: String) {
        self.input.update(&Event::Paste(text));
    }

    /// Tells the person `text` in the transcript.
    pub fn notice(&mut self, text: &str) {
        self.update(&Event::Report(SessionReport::Notice(text.to_owned())));
    }

    /// Carries on the Turn that parked, if the Session has one and an agent to run it.
    pub fn continue_turn(&mut self) {
        self.control(SessionController::continue_parked);
    }

    /// Asks the Session's Turns to stop at the end of their Step, or stops asking.
    #[cfg(test)]
    pub fn set_paused(&self, paused: bool) {
        if let Some(controller) = &self.controller {
            controller.set_paused(paused);
        }
    }

    /// Offers the person the Connections the Session can switch to. Does nothing before the
    /// Session is opened.
    pub fn pick_connection(&mut self) {
        let offer = self.controller.as_ref().and_then(|c| c.connections());
        if let Some(connections) = offer {
            let (names, current) = (connections.names(), connections.current().to_owned());
            self.picker.open(names, &current);
        }
    }

    /// Moves the Session to the Connection called `name` from its next Turn on, and tells the
    /// person whether it did.
    pub fn switch_connection(&mut self, name: &str) {
        let switched = self.control(|controller| controller.switch_connection(name));
        match switched {
            Some(Ok(())) => {
                self.status.set_connection(name);
                self.notice(&format!("runs on {name} from the next Turn"));
            }
            Some(Err(error)) => self.notice(&format!("not switched: {error}")),
            None => {}
        }
    }

    /// Sets what the status line says about other Sessions.
    pub fn set_others(&mut self, others: Option<String>) {
        self.status.set_others(others);
    }

    /// Draws the Session into `area`, with any question for the person on top.
    ///
    /// The input grows with what is typed, up to half of `area`.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let input_height = self.input.height(area.width).min(area.height / 2).max(3);
        let [transcript, status, input] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .areas(area);
        self.transcript.render(frame, transcript);
        self.status.render(frame, status);
        self.input.render(frame, input);
        self.picker.render(frame, area);
        self.approval.render(frame, area);
        self.questions.render(frame, area);
    }
}

/// Returns the first thing the person said among `entries`.
fn first_prompt(entries: &[Entry]) -> Option<String> {
    entries.iter().find_map(|entry| match entry {
        Entry::User { text } => Some(text.clone()),
        _ => None,
    })
}
