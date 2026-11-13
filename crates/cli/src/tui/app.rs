//! The interactive harness as a whole: it keeps every Session, routes events to them and carries
//! out what they ask for.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use bon::bon;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::SetTitle;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::{Frame, Terminal};
use sstack::commands::Command;
use sstack::open_session::{OpenSessions, SessionController};
use sstack::rebuild::Checkout;
use sstack::running_harness::{
    Exit, InSession, Outcome, Phase, Refusal, RunningHarness, SessionState,
};
use sstack::session;
use sstack::session::SessionSummary;
use sstack::vim::{VimEvent, VimFailure};

use super::component::{Component, KeyOutcome};
use super::components::{Activity, Row, SIDEBAR_WIDTH, Sidebar};
use sstack::session_report::SessionReport;

use super::event::{Action, Event, EventReceiver};
use super::pane::{Opened, Pane};
use super::theme::ColorDepth;
use sstack::open_session::{Place, Start};

/// The reason given to the person when a Session cannot be put away during a Turn.
const BUSY: &str = "a Turn is running: wait for it to end, or press Alt-C to cancel it";
/// What the person is told when what they sent waits for the running Turn.
const QUEUED: &str = "queued: the agent reads it after its current Step, and Alt-C gives it back";
/// What the person is told when Queued Prompts come back to the Draft unsent.
const WITHDRAWN: &str = "not sent: what you queued is back in the prompt";
/// The reason given to the person when nothing new starts.
const DRAINING: &str = "the harness is stopping: press Alt-C to stay";
/// The reason given to the person when Alt-C does not quit.
const OTHERS_WORKING: &str = "other Sessions are working: use /quit to stop them too";
/// The narrowest screen that shows the sidebar, which would otherwise crowd out the Session.
const SIDEBAR_MIN_SCREEN_WIDTH: u16 = 80;

/// The keys that work whatever the Session on screen is doing, even with a question open, so the
/// person can always get away to something more urgent. Each is pressed with Alt, the one modifier
/// vim leaves free. Switching Sessions uses E and Y, after vim's Ctrl-E and Ctrl-Y, which scroll
/// down and up.
const GLOBAL_KEYS: [(char, Action); 5] = [
    ('c', Action::Interrupt),
    ('x', Action::Archive),
    ('e', Action::NextSession),
    ('y', Action::PreviousSession),
    ('g', Action::LongestWaiting),
];

/// Everything the [`App`] needs done to Sessions beyond the screen.
pub trait Sessions {
    /// Starts an agent on the Session `start` asks for.
    ///
    /// # Errors
    /// Returns an error if the Session cannot be readied.
    fn open(&mut self, start: Start<'_>) -> Result<Opened>;

    /// Refuses, saying why, if archiving the Session with id `id` would lose work.
    ///
    /// # Errors
    /// Returns an error if there is work to lose, or if that cannot be told.
    fn check_archive(&self, id: &str) -> Result<()>;

    /// Starts putting the Session with id `id` away in the background, deleting its worktree and
    /// any work there, which ends in an [`Event::Archived`]. Returns at once, because deleting a
    /// worktree can take seconds.
    fn archive(&mut self, id: &str);

    /// Stores `name` as the Session Name of the Session with id `id`, so that it lasts beyond this
    /// run. With `None`, the Session is known by its first prompt again.
    ///
    /// # Errors
    /// Returns an error if the name could not be stored.
    fn rename(&mut self, id: &str, name: Option<&str>) -> Result<()>;

    /// Starts building `checkout` of the Harness Source in the background, which ends in an
    /// [`SessionReport::BuildFinished`]. A [`Checkout::Worktree`] means the worktree of the
    /// Session with id `id`.
    fn start_build(&mut self, checkout: Checkout, id: &str);

    /// Stops the build, if one is running.
    fn cancel_build(&mut self);
}

/// The interactive harness: the one place that knows every Session and what each [`Action`]
/// means.
pub struct App<'a> {
    /// Every Session the person can switch to, in no meaningful order.
    panes: Vec<Pane>,
    /// The position in `panes` of the Session on screen.
    shown: usize,
    /// Sessions taken off the list while they are put away in the background, kept so that one
    /// that cannot be put away can come back.
    archiving: Vec<Pane>,
    /// The rules every open Session follows together, such as on the way to a Rebuild.
    harness: RunningHarness,
    /// The list of Sessions.
    sidebar: Sidebar,
    /// Where Sessions are opened and archived as the person asks.
    sessions: Box<dyn Sessions + 'a>,
    /// How many colors the terminal can show, which the theme's colors are fitted to.
    colors: ColorDepth,
    /// The cursor shape the terminal was last told to use.
    cursor_style: SetCursorStyle,
    /// The title the terminal was last told to show. Empty until it is first told.
    title: String,
    /// Whether the person has heard that nvim is missing, which every Session would otherwise
    /// repeat.
    told_nvim_missing: bool,
}

#[bon]
impl<'a> App<'a> {
    /// Creates the harness.
    #[builder]
    pub fn new(
        /// The Session to show, which may also be one of `stored`.
        first: Opened,
        /// The Sessions a key press away.
        stored: Vec<SessionSummary>,
        /// Starts the Sessions the person turns to.
        sessions: Box<dyn Sessions + 'a>,
        /// How many colors the terminal can show.
        colors: ColorDepth,
    ) -> Self {
        let mut panes: Vec<Pane> = stored.into_iter().map(Pane::stored).collect();
        let found = panes.iter().position(|pane| pane.path() == first.path);
        let shown = found.unwrap_or(panes.len());
        match panes.get_mut(shown) {
            Some(pane) => pane.attach(first),
            None => panes.push(Pane::opened(first)),
        }
        Self {
            panes,
            shown,
            archiving: Vec::new(),
            harness: RunningHarness::default(),
            sidebar: Sidebar::default(),
            sessions,
            colors,
            cursor_style: SetCursorStyle::DefaultUserShape,
            title: String::new(),
            told_nvim_missing: false,
        }
    }
}

impl App<'_> {
    /// Returns where the Session on screen is stored.
    pub fn session(&self) -> &Path {
        self.panes[self.shown].path()
    }

    /// Runs until the person quits or asks for a Rebuild, redrawing `terminal` whenever events
    /// have been handled.
    ///
    /// Also quits once nothing can send events any more.
    ///
    /// # Errors
    /// Returns an error if drawing to the terminal fails.
    pub async fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        events: &mut EventReceiver,
    ) -> Result<Exit, B::Error> {
        loop {
            terminal.draw(|frame| self.render(frame))?;
            self.apply_cursor_style();
            self.apply_title();
            let Some(event) = events.recv().await else {
                return Ok(Exit::Quit);
            };
            if let Some(exit) = self.handle_waiting(event, events) {
                return Ok(exit);
            }
        }
    }

    /// Gives the terminal's cursor the shape the Session on screen calls for. Goes straight to the
    /// terminal, because the screen library has no notion of cursor shape.
    fn apply_cursor_style(&mut self) {
        let wanted = self.panes[self.shown].cursor_style();
        if wanted != self.cursor_style {
            self.cursor_style = wanted;
            let _ = execute!(io::stdout(), wanted);
        }
    }

    /// Titles the terminal after the Session on screen. Goes straight to the terminal, because the
    /// screen library has no notion of a title.
    fn apply_title(&mut self) {
        let wanted = session::window_title(self.panes[self.shown].known_name().as_deref());
        if wanted != self.title {
            let _ = execute!(io::stdout(), SetTitle(&wanted));
            self.title = wanted;
        }
    }

    /// Handles `first` and every event already waiting behind it, so that streamed output is
    /// drawn once per batch rather than once per piece.
    fn handle_waiting(&mut self, first: Event, events: &mut EventReceiver) -> Option<Exit> {
        let mut next = Some(first);
        while let Some(event) = next {
            if let Some(exit) = self.handle(event) {
                return Some(exit);
            }
            next = events.try_recv().ok();
        }
        None
    }

    /// Reacts to one event, returning why to stop if it calls for that.
    pub fn handle(&mut self, event: Event) -> Option<Exit> {
        match event {
            Event::Key(key) => return self.key(key),
            Event::Session(id, event) => return self.pass_on(&id, &event),
            Event::Report(SessionReport::BuildFinished(outcome)) => {
                return self.build_finished(outcome);
            }
            Event::Click { column, row } => self.click(column, row),
            Event::Archived { id, outcome } => self.archived(&id, outcome),
            event => self.panes[self.shown].update(&event),
        }
        None
    }

    /// Passes on something the Session with id `id` did, returning why to stop if it calls for
    /// that.
    fn pass_on(&mut self, id: &str, event: &Event) -> Option<Exit> {
        let index = self.position(id)?;
        self.panes[index].observe(event, index == self.shown);
        match event {
            Event::Vim(VimEvent::Submitted(line)) => return self.vim_submitted(index, line),
            Event::Vim(VimEvent::Lost { failure, .. }) => self.vim_lost(index, failure),
            Event::Report(report) => return self.drive(index, Happened::Reported(report)),
            _ => {}
        }
        None
    }

    /// Has the Running Harness act on what `happened` in the Session at position `index`, and
    /// carries out what comes of it. Does nothing to a Session with no agent yet.
    fn drive(&mut self, index: usize, happened: Happened<'_>) -> Option<Exit> {
        let (before, rest) = self.panes.split_at_mut(index);
        let (pane, after) = rest.split_first_mut()?;
        let harness = &mut self.harness;
        let mut others = Panes(before, after);
        let outcome = pane.control(|focus| happened.act(harness.in_session(focus, &mut others)));
        self.carry_out(index, outcome.flatten()?)
    }

    /// Does what the Running Harness asked for after acting on the Session at position `index`.
    fn carry_out(&mut self, index: usize, outcome: Outcome<'_>) -> Option<Exit> {
        match outcome {
            Outcome::TurnStarted(text) => self.panes[index].turn_started(&text),
            Outcome::Queued => self.panes[index].notice(QUEUED),
            Outcome::Withdrawn(text) => {
                self.panes[index].give_back(text);
                self.panes[index].notice(WITHDRAWN);
            }
            Outcome::Refused(refusal) => self.notice(refusal_text(&refusal)),
            Outcome::Command(command) => self.command(&command),
            Outcome::StartBuild(checkout) => {
                let id = self.panes[index].id().to_owned();
                self.sessions.start_build(checkout, &id);
            }
            Outcome::CancelBuild => self.sessions.cancel_build(),
            Outcome::Exit(exit) => return Some(exit),
        }
        None
    }

    /// Carries out a Human Command that is about how the harness keeps its Sessions.
    fn command(&mut self, command: &Command<'_>) {
        match *command {
            Command::New => self.open_new(Place::Worktree),
            Command::NewMain => self.open_new(Place::MainCheckout),
            Command::Archive { force } => self.archive(force),
            Command::Vim => self.panes[self.shown].toggle_vim(),
            Command::Rename(name) => self.rename(name),
            Command::Connection(name) => self.connection(name),
            _ => {}
        }
    }

    /// Moves the Session on screen to the Connection called `name` from its next Turn on, or
    /// with `None` offers the Connections to choose from.
    fn connection(&mut self, name: Option<&str>) {
        if let Err(error) = self.ensure_agent(self.shown) {
            self.notice(format!("could not open the Session: {error}"));
            return;
        }
        let pane = &mut self.panes[self.shown];
        match name {
            Some(name) => pane.switch_connection(name),
            None => pane.pick_connection(),
        }
    }

    /// Carries on every Turn an earlier run left parked. A Session that cannot open stays parked
    /// and says so.
    pub fn continue_parked(&mut self) {
        for index in 0..self.panes.len() {
            if self.panes[index].state() != SessionState::Parked {
                continue;
            }
            match self.ensure_agent(index) {
                Ok(()) => self.panes[index].continue_turn(),
                Err(error) => self.notice(format!("could not carry on a Session: {error}")),
            }
        }
    }

    /// Acts on the end of the build.
    fn build_finished(&mut self, outcome: Result<PathBuf, String>) -> Option<Exit> {
        let binary = outcome.map_err(|failure| self.notice(failure)).ok();
        let all = &mut Panes(&mut self.panes, &mut []);
        let outcome = self.harness.build_finished(binary, all)?;
        self.carry_out(self.shown, outcome)
    }

    /// Tells the person that the relaunch failed, and lets every Session carry on here.
    pub fn relaunch_failed(&mut self, failure: &str) {
        self.notice(format!("rebuild failed: {failure}"));
        let all = &mut Panes(&mut self.panes, &mut []);
        self.harness.relaunch_failed(all);
    }

    /// Acts on a line the Session at position `index` submitted through nvim. The person may have
    /// switched away while nvim answered, so that Session comes back to take its line.
    fn vim_submitted(&mut self, index: usize, line: &str) -> Option<Exit> {
        if index != self.shown {
            self.show(index);
        }
        self.submit(line)
    }

    /// Tells the person that the Session at position `index` is back to the plain prompt.
    fn vim_lost(&mut self, index: usize, failure: &VimFailure) {
        let missing = *failure == VimFailure::NotInstalled;
        if missing && self.told_nvim_missing {
            return;
        }
        self.told_nvim_missing |= missing;
        let notice = format!("typing into the plain prompt: {failure}. /vim tries again");
        self.panes[index].update(&Event::Report(SessionReport::Notice(notice)));
    }

    /// Returns the position in `panes` of the Session with id `id`.
    fn position(&self, id: &str) -> Option<usize> {
        self.panes.iter().position(|pane| pane.id() == id)
    }

    /// Shows the Session drawn at this screen position, if there is one.
    fn click(&mut self, column: u16, row: u16) {
        let clicked = self.sidebar.session_at(column, row);
        if let Some(index) = clicked.and_then(|id| self.position(id)) {
            self.show(index);
        }
    }

    /// Offers a key press to the Session on screen, after the keys that work everywhere.
    fn key(&mut self, key: KeyEvent) -> Option<Exit> {
        if let Some(action) = global_action(key) {
            return self.act(action);
        }
        match self.panes[self.shown].key(key) {
            KeyOutcome::Act(action) => self.act(action),
            _ => None,
        }
    }

    /// Carries out what a component asked for.
    fn act(&mut self, action: Action) -> Option<Exit> {
        match action {
            Action::Submit(line) => return self.submit(&line),
            Action::Interrupt => return self.drive(self.shown, Happened::Interrupted),
            Action::ApprovalAnswered => self.panes[self.shown].answered(),
            Action::QuestionsAnswered(note) => self.panes[self.shown].questions_answered(note),
            Action::NextSession => self.step(1),
            Action::PreviousSession => self.step(-1),
            Action::LongestWaiting => self.show_longest_waiting(),
            Action::Archive => self.archive(false),
            Action::SwitchConnection(name) => self.connection(Some(&name)),
        }
        None
    }

    /// Returns the position in `panes` of each Session, in the order the sidebar lists them.
    fn queue(&self) -> Vec<usize> {
        let mut queue: Vec<usize> = (0..self.panes.len()).collect();
        queue.sort_by_key(|index| self.panes[*index].queue_position());
        queue
    }

    /// Shows the Session `offset` places along the sidebar from this one, wrapping at its ends.
    fn step(&mut self, offset: isize) {
        let queue = self.queue();
        let here = queue.iter().position(|index| *index == self.shown);
        let here = here.unwrap_or_default().cast_signed();
        let length = queue.len().cast_signed();
        self.show(queue[(here + offset).rem_euclid(length).cast_unsigned()]);
    }

    /// Shows the Session that has waited longest for the person, besides the one on screen, which
    /// they have already got to.
    fn show_longest_waiting(&mut self) {
        let next = self
            .queue()
            .into_iter()
            .find(|index| *index != self.shown && self.panes[*index].state().is_waiting());
        if let Some(index) = next {
            self.show(index);
        }
    }

    /// Puts the Session at position `index` on screen, starting its agent if it has none yet. If
    /// that fails, the current Session stays and the person is told why.
    fn show(&mut self, index: usize) {
        match self.ensure_agent(index) {
            Ok(()) => {
                self.shown = index;
                self.panes[index].viewed();
            }
            Err(error) => self.notice(format!("could not open the Session: {error}")),
        }
    }

    /// Starts an agent for the Session at position `index`, unless it already has one.
    fn ensure_agent(&mut self, index: usize) -> Result<()> {
        if !self.panes[index].has_agent() {
            let opened = self.sessions.open(Start::Resume(self.panes[index].id()))?;
            self.panes[index].attach(opened);
        }
        Ok(())
    }

    /// Acts on a line the person entered.
    fn submit(&mut self, line: &str) -> Option<Exit> {
        self.drive(self.shown, Happened::Entered(line))
    }

    /// Makes `name` the Session Name of the Session on screen, or with `None` goes back to its first
    /// prompt. If the name cannot be stored, nothing changes and the person is told why.
    fn rename(&mut self, name: Option<&str>) {
        let pane = &mut self.panes[self.shown];
        match self.sessions.rename(pane.id(), name) {
            Ok(()) => pane.rename(name),
            Err(error) => self.notice(format!("not renamed: {error}")),
        }
    }

    /// Starts a new Session working in `place` and puts it on screen, leaving the others running.
    fn open_new(&mut self, place: Place) {
        match self.sessions.open(Start::New(place)) {
            Ok(opened) => {
                self.panes.push(Pane::opened(opened));
                self.shown = self.panes.len() - 1;
            }
            Err(error) => self.notice(format!("could not start a new Session: {error}")),
        }
    }

    /// Puts the Session on screen away and moves on to the next one, unless `force` is false and
    /// that would lose work.
    fn archive(&mut self, force: bool) {
        if self.harness.phase().is_draining() {
            self.notice(DRAINING.to_owned());
            return;
        }
        if self.panes[self.shown].state().in_turn() {
            self.notice(BUSY.to_owned());
            return;
        }
        let leaving = self.shown;
        let id = self.panes[leaving].id().to_owned();
        let allowed = if force {
            Ok(())
        } else {
            self.sessions.check_archive(&id)
        };
        if let Err(error) = allowed {
            self.notice(format!("not archived: {error}"));
            return;
        }
        self.move_on();
        if self.shown != leaving {
            self.put_away(leaving, &id);
        }
    }

    /// Shows the next Session in the queue, or a new one if there are no others. If that fails,
    /// the Session on screen stays.
    fn move_on(&mut self) {
        let shown = self.shown;
        match self.queue().into_iter().find(|index| *index != shown) {
            Some(next) => self.show(next),
            None => self.open_new(Place::Worktree),
        }
    }

    /// Takes the Session at position `leaving`, whose id is `id`, off the list now that it is off
    /// screen, and starts putting it away.
    fn put_away(&mut self, leaving: usize, id: &str) {
        self.sessions.archive(id);
        self.archiving.push(self.panes.remove(leaving));
        if self.shown > leaving {
            self.shown -= 1;
        }
    }

    /// Acts on the end of putting away the Session with id `id`: forgets it, or if that failed,
    /// lists it again and says why.
    fn archived(&mut self, id: &str, outcome: Result<(), String>) {
        let Some(index) = self.archiving.iter().position(|pane| pane.id() == id) else {
            return;
        };
        let pane = self.archiving.remove(index);
        if let Err(error) = outcome {
            self.panes.push(pane);
            self.notice(format!("not archived: {error}"));
        }
    }

    /// Tells the person something from the harness itself, in the Session on screen.
    pub fn notice(&mut self, text: String) {
        self.panes[self.shown].update(&Event::Report(SessionReport::Notice(text)));
    }

    /// Draws the sidebar, if there is room, and the Session on screen beside it.
    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let sidebar_width = if area.width >= SIDEBAR_MIN_SCREEN_WIDTH {
            SIDEBAR_WIDTH
        } else {
            0
        };
        let [sidebar, session] =
            Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(0)]).areas(area);
        self.refresh_sidebar(sidebar);
        self.sidebar.render(frame, sidebar);
        self.panes[self.shown].render(frame, session);
        self.colors.fit_buffer(frame.buffer_mut());
    }

    /// Brings the sidebar up to date. When it has no room, the status line says what it would
    /// have, so that nobody is left waiting unnoticed.
    fn refresh_sidebar(&mut self, area: Rect) {
        let now = SystemTime::now();
        let queue = self.queue();
        let rows: Vec<Row> = queue
            .iter()
            .map(|index| self.panes[*index].row(now))
            .collect();
        let on_screen = queue.iter().position(|index| *index == self.shown);
        let running = self.panes.iter().filter(|pane| pane.state().in_turn());
        let progress = progress(self.harness.phase(), running.count());
        let others = (area.is_empty())
            .then(|| progress.clone().or_else(|| self.summary_of_others(&rows)))
            .flatten();
        self.panes[self.shown].set_others(others);
        self.sidebar.set_progress(progress);
        self.sidebar.place(area);
        self.sidebar.list(rows, on_screen.unwrap_or_default());
    }

    /// Sums up what Sessions other than the one on screen are doing, if there is anything to say.
    fn summary_of_others(&self, rows: &[Row]) -> Option<String> {
        let id = self.panes[self.shown].id();
        let count = |activity| {
            let others = rows.iter().filter(|row| row.id != id);
            others.filter(|row| row.activity == activity).count()
        };
        let parts: Vec<String> = [
            (Activity::Blocked, "blocked"),
            (Activity::Working, "working"),
        ]
        .into_iter()
        .map(|(activity, name)| (count(activity), name))
        .filter(|(count, _)| *count > 0)
        .map(|(count, name)| format!("{count} {name}"))
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// Something that happened in one Session, which the Running Harness acts on.
enum Happened<'a> {
    /// The person entered this line.
    Entered(&'a str),
    /// Core reported this about the Session.
    Reported(&'a SessionReport),
    /// The person asked to interrupt while looking at the Session.
    Interrupted,
}

impl<'a> Happened<'a> {
    /// Has `harness` act on what happened, returning what the App has to do about it.
    fn act(self, harness: InSession<'_>) -> Option<Outcome<'a>> {
        match self {
            Self::Entered(line) => harness.submit(line),
            Self::Reported(report) => harness.report(report),
            Self::Interrupted => harness.interrupt(),
        }
    }
}

/// The open Sessions among two runs of panes, which the Running Harness reaches through
/// [`OpenSessions`].
struct Panes<'p>(&'p mut [Pane], &'p mut [Pane]);

impl OpenSessions for Panes<'_> {
    fn visit(&mut self, visit: &mut dyn FnMut(&mut SessionController)) {
        for pane in self.0.iter_mut().chain(self.1.iter_mut()) {
            pane.control(&mut *visit);
        }
    }
}

/// Returns what to tell the person when the Running Harness refused to act, and why.
fn refusal_text(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Busy => BUSY.to_owned(),
        Refusal::Draining => DRAINING.to_owned(),
        Refusal::OthersWorking => OTHERS_WORKING.to_owned(),
        Refusal::UnknownCommand(line) => format!("unknown command: {line}"),
        Refusal::AmbiguousSkill { name, names } => {
            format!(
                "/{name} could mean {}: type the full name",
                names.join(" or ")
            )
        }
        Refusal::UnreadableSkill { name, error } => {
            format!("could not load the Skill {name}: {error}")
        }
    }
}

/// Returns what the Running Harness is doing in `phase`, with `waiting_for` Turns still running,
/// if the person should know.
fn progress(phase: &Phase, waiting_for: usize) -> Option<String> {
    let doing = match phase {
        Phase::Running => return None,
        Phase::DrainingToQuit => "quitting",
        Phase::DrainingToRebuild { binary: None } => "building",
        Phase::DrainingToRebuild { binary: Some(_) } => "built",
    };
    Some(format!("{doing}, {waiting_for} running, Alt-C stops"))
}

/// Returns what a key press does whatever is on screen, if anything.
fn global_action(key: KeyEvent) -> Option<Action> {
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    GLOBAL_KEYS
        .into_iter()
        .find(|(bound, _)| *bound == character && key.modifiers == KeyModifiers::ALT)
        .map(|(_, action)| action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::event::EventSender;
    use crate::tui::test_support::{
        alt, ask_call, bash_call, ctrl, drawn, key, opened, reply, rows,
    };
    use anyhow::{anyhow, bail};
    use async_trait::async_trait;
    use insta::assert_snapshot;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use sstack::connection::{
        ModelRequest, Provider, ProviderError, StreamEvent,
        scripted::{Scripted, ScriptedProvider},
    };
    use sstack::session::{self, AssistantEntry, Entry, SessionLog, StopReason, ToolCall};
    use sstack::vim::Draft;
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::{Semaphore, mpsc};

    /// A width that leaves room for the sidebar.
    const WIDE: u16 = 100;
    /// A width too narrow for the sidebar.
    const NARROW: u16 = 60;

    /// An [`App`] wired to real agents, with the events it would receive held back for the test.
    struct Harness {
        dir: TempDir,
        app: App<'static>,
        sender: EventSender,
        events: EventReceiver,
        /// Where each new Session was asked to work.
        places: Rc<RefCell<Vec<Place>>>,
        /// The ids of the Sessions archived so far.
        archived: Rc<RefCell<Vec<String>>>,
        /// What has happened to builds so far.
        builds: Rc<RefCell<Vec<String>>>,
    }

    /// Returns where the Session with id `id` is stored in `dir`.
    fn path_of(dir: &TempDir, id: &str) -> PathBuf {
        dir.path().join(id).with_extension("jsonl")
    }

    /// Sessions that all run on one [`Provider`], numbering new ones from one.
    struct TestSessions {
        /// Where Sessions are stored.
        root: PathBuf,
        /// The Provider that runs every Session's Turns.
        provider: Arc<dyn Provider>,
        /// Where every Session reports.
        sender: EventSender,
        /// How many new Sessions have been started.
        created: usize,
        /// Where each new Session was asked to work.
        places: Rc<RefCell<Vec<Place>>>,
        /// The ids of the Sessions archived so far.
        archived: Rc<RefCell<Vec<String>>>,
        /// What has happened to builds so far.
        builds: Rc<RefCell<Vec<String>>>,
        /// Why opening fails, if it does.
        open_failure: Option<&'static str>,
        /// Why archiving is refused, if it is.
        refusal: Option<&'static str>,
        /// Whether archives stay unfinished until the test says how they went. Otherwise each one
        /// reports success on the event channel.
        hold_archives: bool,
        /// Why storing a Session Name fails, if it does.
        rename_failure: Option<&'static str>,
    }

    impl Sessions for TestSessions {
        fn open(&mut self, start: Start<'_>) -> Result<Opened> {
            if let Some(failure) = self.open_failure {
                bail!(failure);
            }
            let id = match start {
                Start::Resume(id) => id.to_owned(),
                Start::New(place) => {
                    self.places.borrow_mut().push(place);
                    self.created += 1;
                    format!("new{}", self.created)
                }
            };
            let path = self.root.join(id).with_extension("jsonl");
            Ok(opened(&path, self.provider.clone(), &self.sender))
        }

        fn check_archive(&self, _id: &str) -> Result<()> {
            self.refusal.map_or(Ok(()), |refusal| Err(anyhow!(refusal)))
        }

        fn archive(&mut self, id: &str) {
            self.archived.borrow_mut().push(id.to_owned());
            if !self.hold_archives {
                let _ = self.sender.send(archived(id, Ok(())));
            }
        }

        fn rename(&mut self, id: &str, name: Option<&str>) -> Result<()> {
            if let Some(failure) = self.rename_failure {
                bail!(failure);
            }
            Ok(session::name_session(&self.root, id, name)?)
        }

        fn start_build(&mut self, checkout: Checkout, id: &str) {
            let started = match checkout {
                Checkout::Main => "started".to_owned(),
                Checkout::Worktree => format!("started the worktree of {id}"),
            };
            self.builds.borrow_mut().push(started);
        }

        fn cancel_build(&mut self) {
            self.builds.borrow_mut().push("cancelled".to_owned());
        }
    }

    /// Runs `check` with every age in the sidebar under a minute shown as `Ns`, because how many
    /// seconds a Session has waited depends on how fast the test runs.
    fn hiding_seconds(check: impl FnOnce()) {
        let mut settings = insta::Settings::clone_current();
        settings.add_filter(r"\d+s\x{2502}", "Ns\u{2502}");
        settings.bind(check);
    }

    fn harness(provider: Arc<dyn Provider>) -> Harness {
        start(tempfile::tempdir().unwrap(), provider, vec![])
    }

    /// Starts an [`App`] in `dir` showing a new Session, with `stored` a key press away.
    fn start(dir: TempDir, provider: Arc<dyn Provider>, stored: Vec<SessionSummary>) -> Harness {
        start_with()
            .dir(dir)
            .provider(provider)
            .stored(stored)
            .adjust(|_| {})
            .call()
    }

    /// Starts an [`App`] like [`start`], with `adjust` applied to its Sessions first.
    #[bon::builder]
    fn start_with(
        /// Where the Sessions are stored.
        dir: TempDir,
        /// Answers every Session.
        provider: Arc<dyn Provider>,
        /// The Sessions a key press away.
        #[builder(default)]
        stored: Vec<SessionSummary>,
        /// Sets up how the Sessions misbehave.
        adjust: impl FnOnce(&mut TestSessions),
    ) -> Harness {
        let (sender, events) = mpsc::unbounded_channel();
        let first = opened(&path_of(&dir, "first"), provider.clone(), &sender);
        let places = Rc::default();
        let archived = Rc::default();
        let builds = Rc::default();
        let mut sessions = TestSessions {
            root: dir.path().to_owned(),
            provider,
            sender: sender.clone(),
            created: 0,
            places: Rc::clone(&places),
            archived: Rc::clone(&archived),
            builds: Rc::clone(&builds),
            open_failure: None,
            refusal: None,
            hold_archives: false,
            rename_failure: None,
        };
        adjust(&mut sessions);
        let app = App::builder()
            .first(first)
            .stored(stored)
            .sessions(Box::new(sessions))
            .colors(ColorDepth::Ansi256)
            .build();
        Harness {
            dir,
            app,
            sender,
            events,
            places,
            archived,
            builds,
        }
    }

    fn scripted(script: Vec<Scripted>) -> Harness {
        harness(Arc::new(ScriptedProvider::new(script)))
    }

    impl Harness {
        /// Replaces the [`App`] with a new one on the same Session, as a Rebuild does.
        async fn rebuilt(self) -> Harness {
            let dir = self.stopped().await;
            start(dir, Arc::new(ScriptedProvider::new(vec![])), vec![])
        }

        /// Stops the [`App`] as a relaunch does, returning its project once every Session it
        /// had open is closed and free to open again.
        async fn stopped(self) -> TempDir {
            let Self {
                dir,
                app,
                sender,
                mut events,
                ..
            } = self;
            drop((app, sender));
            while events.recv().await.is_some() {}
            dir
        }

        /// Presses a key that goes to the [`App`] rather than to the prompt.
        fn press(&mut self, character: char) -> Option<Exit> {
            self.app.handle(Event::Key(alt(character)))
        }

        /// Starts a new Session, which leaves the keyboard's prompt ready for it.
        fn enter_new(&mut self) {
            assert_eq!(self.enter("/new"), None);
        }

        /// Handles events from the agents until one stops the harness.
        async fn until_exit(&mut self) -> Exit {
            loop {
                let event = self.events.recv().await.unwrap();
                if let Some(exit) = self.app.handle(event) {
                    return exit;
                }
            }
        }

        /// Starts a Turn that will ask about `touch ran`, then asks for `command` from a new
        /// Session, so that the question does not catch what is typed.
        async fn drain_with_a_question_open(&mut self, command: &str) -> Option<Exit> {
            self.enter("go");
            self.enter_new();
            let exit = self.enter(command);
            self.until(|event| matches!(event, Event::Report(SessionReport::Approval(_))))
                .await;
            exit
        }

        /// Starts a Turn whose model is held back, then asks for `command` from a new Session, so
        /// that a Step is in progress when `command` arrives.
        async fn drain_with_a_step_running(&mut self, command: &str) -> Option<Exit> {
            self.enter("go");
            self.until(|event| matches!(event, Event::Report(SessionReport::TurnStarted(_))))
                .await;
            self.enter_new();
            self.enter(command)
        }

        /// Handles events from the agents until a Session asks a question, then answers yes to
        /// it.
        async fn until_asked_then_approve(&mut self) {
            self.until(|event| matches!(event, Event::Report(SessionReport::Approval(_))))
                .await;
            self.approve();
        }

        /// Answers yes to the question on the Session that has waited longest.
        fn approve(&mut self) {
            self.press('g');
            self.app.handle(Event::Key(key(KeyCode::Char('y'))));
        }

        /// Returns the id of the Session on screen.
        fn shown(&self) -> String {
            self.app.panes[self.app.shown].id().to_owned()
        }

        /// Types `line` at the prompt and presses Enter.
        fn enter(&mut self, line: &str) -> Option<Exit> {
            for character in line.chars() {
                assert_eq!(
                    self.app.handle(Event::Key(key(KeyCode::Char(character)))),
                    None
                );
            }
            self.app.handle(Event::Key(key(KeyCode::Enter)))
        }

        /// Passes on a report from the nvim of the Session on screen.
        fn vim_reports(&mut self, event: VimEvent) -> Option<Exit> {
            let event = Box::new(Event::Vim(event));
            self.app.handle(Event::Session(self.shown(), event))
        }

        /// Handles events from the agent until one matching `last` has been handled.
        async fn until(&mut self, last: impl Fn(&Event) -> bool) {
            loop {
                let event = self.events.recv().await.unwrap();
                let Event::Session(_, inner) = &event else {
                    panic!("expected an event from a Session");
                };
                let done = last(inner);
                assert_eq!(self.app.handle(event), None);
                if done {
                    return;
                }
            }
        }

        async fn until_turn_ends(&mut self) {
            self.until(|event| matches!(event, Event::Report(SessionReport::TurnEnded(_))))
                .await;
        }

        /// Draws the whole screen `width` columns wide, returning it ready for a snapshot.
        fn drawn_at(&mut self, width: u16) -> TestBackend {
            let terminal = drawn(width, 12, |frame| self.app.render(frame));
            terminal.backend().clone()
        }

        fn screen_of(&mut self, width: u16) -> String {
            let terminal = drawn(width, 12, |frame| self.app.render(frame));
            rows(&terminal).join("\n")
        }

        fn screen(&mut self) -> String {
            self.screen_of(NARROW)
        }

        fn session(&self) -> Vec<Entry> {
            session::lock_free_read(&path_of(&self.dir, "first")).unwrap()
        }
    }

    #[tokio::test]
    async fn prompts_run_turns_and_their_replies_are_shown() {
        let mut h = scripted(vec![Ok(reply("hello back"))]);
        assert_eq!(h.enter("hello"), None);
        h.until_turn_ends().await;

        assert_snapshot!(h.drawn_at(NARROW));
        assert_eq!(h.session().len(), 2);
    }

    #[tokio::test]
    async fn after_a_rebuild_the_session_so_far_is_shown() {
        let mut h = scripted(vec![Ok(reply("hello back"))]);
        h.enter("hello");
        h.until_turn_ends().await;

        let mut h = h.rebuilt().await;
        h.until(|event| matches!(event, Event::Report(SessionReport::Resumed(_))))
            .await;

        assert_snapshot!(h.drawn_at(NARROW));
    }

    /// Starts an [`App`] answered by `script`, whose first Session has a project Skill for each
    /// directory name and `SKILL.md` text in `skills`.
    fn with_skills(skills: &[(&str, &str)], script: Vec<Scripted>) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in skills {
            let directory = dir.path().join(".agents/skills").join(name);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("SKILL.md"), text).unwrap();
        }
        start(dir, Arc::new(ScriptedProvider::new(script)), vec![])
    }

    #[tokio::test]
    async fn a_skill_runs_by_name_with_what_the_person_adds() {
        let skill = ("commits", "---\nname: commits\n---\nKeep titles short.\n");
        let mut h = with_skills(&[skill], vec![Ok(reply("done"))]);

        assert_eq!(h.enter("/commits be brief"), None);
        h.until_turn_ends().await;

        let Entry::User { text } = &h.session()[0] else {
            panic!("the Turn did not start with the person's message");
        };
        assert!(text.contains("Keep titles short."), "{text}");
        assert!(!text.contains("name: commits"), "{text}");
        assert!(text.ends_with("</skill>\n\nbe brief"), "{text}");
    }

    #[tokio::test]
    async fn a_bare_name_that_only_names_skills_with_their_scope_runs_none() {
        let skill = ("review", "---\nname: project:review\n---\nReview.\n");
        let mut h = with_skills(&[skill], vec![]);

        assert_eq!(h.enter("/review"), None);

        let screen = h.screen();
        assert!(
            screen.contains("/review could mean project:review"),
            "{screen}"
        );
        assert_eq!(h.session(), Vec::new());
    }

    #[tokio::test]
    async fn human_commands_never_reach_the_model() {
        let mut h = scripted(vec![]);
        assert_eq!(h.enter(""), None);
        assert_eq!(h.enter("/nope"), None);
        assert!(h.screen().contains("unknown command: /nope"));
        assert_eq!(h.enter("/quit"), Some(Exit::Quit));
        assert_eq!(h.session(), Vec::new());
    }

    #[tokio::test]
    async fn an_approved_command_runs_and_the_turn_carries_on() {
        let script = vec![Ok(bash_call("touch ran")), Ok(reply("all done"))];
        let mut h = scripted(script);
        h.enter("go");
        h.until(|event| matches!(event, Event::Report(SessionReport::Approval(_))))
            .await;
        assert_snapshot!(h.drawn_at(NARROW));

        assert_eq!(h.app.handle(Event::Key(key(KeyCode::Char('y')))), None);
        h.until_turn_ends().await;

        assert!(h.dir.path().join("ran").exists());
        assert_snapshot!(h.drawn_at(NARROW));
    }

    #[tokio::test]
    async fn the_models_questions_are_answered_and_the_answers_reach_it() {
        let script = vec![
            Ok(ask_call("Which store?", &["Files", "SQLite"])),
            Ok(reply("noted")),
        ];
        let mut h = scripted(script);
        h.enter("grill me");
        h.until(|event| matches!(event, Event::Report(SessionReport::Question(_))))
            .await;
        assert_eq!(h.app.panes[0].state(), SessionState::AwaitingAnswer);
        assert_snapshot!(h.drawn_at(NARROW));

        h.app.handle(Event::Key(key(KeyCode::Down)));
        h.app.handle(Event::Key(key(KeyCode::Enter)));
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_turn_ends().await;

        let answered = h.session().into_iter().any(|entry| {
            matches!(entry, Entry::ToolResult { output, is_error: false, .. } if output == "Pick: SQLite")
        });
        assert!(answered, "{:#?}", h.session());
        assert!(h.screen().contains("[answered] Pick: SQLite"));
    }

    /// A Provider that never answers, so that a Turn stays running until it is cancelled.
    struct NeverResponds;

    #[async_trait]
    impl Provider for NeverResponds {
        async fn respond(
            &self,
            _request: ModelRequest<'_>,
            _on_event: &mut (dyn FnMut(StreamEvent) + Send),
        ) -> Result<AssistantEntry, ProviderError> {
            std::future::pending().await
        }
    }

    /// A Provider that gives scripted responses, each only once the test lets it through, so
    /// that a Step stays in progress for as long as the test needs.
    struct Gated {
        /// The responses, in order.
        script: ScriptedProvider,
        /// One permit for each response the test has let through and the Provider has not given
        /// yet.
        gate: Arc<Semaphore>,
    }

    #[async_trait]
    impl Provider for Gated {
        async fn respond(
            &self,
            request: ModelRequest<'_>,
            on_event: &mut (dyn FnMut(StreamEvent) + Send),
        ) -> Result<AssistantEntry, ProviderError> {
            self.gate.acquire().await.unwrap().forget();
            self.script.respond(request, on_event).await
        }
    }

    /// Starts an [`App`] whose model gives the `script`ed responses, each only once a permit is
    /// added to the returned gate.
    fn gated(script: Vec<Scripted>) -> (Harness, Arc<Semaphore>) {
        let gate = Arc::new(Semaphore::new(0));
        let provider = Gated {
            script: ScriptedProvider::new(script),
            gate: gate.clone(),
        };
        (harness(Arc::new(provider)), gate)
    }

    /// Returns a model response asking to read a file that is not there, which runs without
    /// asking the person.
    fn read_call() -> AssistantEntry {
        AssistantEntry {
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read".into(),
                input: serde_json::json!({ "path": "missing" }),
            }],
            stop: StopReason::ToolUse,
            ..reply("")
        }
    }

    #[tokio::test]
    async fn while_a_turn_runs_ctrl_c_cancels_it_and_gives_back_what_was_queued() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter("go");
        h.until(|event| matches!(event, Event::Report(SessionReport::TurnStarted(_))))
            .await;
        assert!(h.screen().contains("working"));

        assert_eq!(h.enter("again"), None);
        assert!(h.screen().contains("queued"));

        assert_eq!(h.app.handle(Event::Key(alt('c'))), None);
        h.until_turn_ends().await;
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("[cancelled]"), "{screen}");
        assert!(screen.contains("> again"), "{screen}");
        assert_eq!(h.session().last(), Some(&Entry::TurnCancelled));

        assert_eq!(h.app.handle(Event::Key(alt('c'))), Some(Exit::Quit));
    }

    #[tokio::test]
    async fn a_prompt_sent_during_a_step_reaches_the_model_after_it() {
        let (mut h, gate) = gated(vec![Ok(read_call()), Ok(reply("all done"))]);
        h.enter("go");
        h.until(|event| matches!(event, Event::Report(SessionReport::TurnStarted(_))))
            .await;

        assert_eq!(h.enter("also this"), None);
        gate.add_permits(2);
        h.until_turn_ends().await;

        let queued = Entry::User {
            text: "also this".into(),
        };
        assert_eq!(h.session()[3], queued);
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("also this"), "{screen}");
        assert!(screen.contains("all done"), "{screen}");
    }

    #[tokio::test]
    async fn the_prompt_grows_with_long_input_up_to_half_the_screen() {
        let mut h = scripted(vec![]);
        let typed = "word ".repeat(40);
        h.app.handle(Event::Paste(typed.clone()));
        assert_snapshot!(h.drawn_at(NARROW));

        h.app.handle(Event::Paste("\n".repeat(20)));
        assert_snapshot!(h.drawn_at(NARROW));
    }

    #[tokio::test]
    async fn the_screen_uses_only_colors_the_terminal_can_show() {
        let mut h = scripted(vec![]);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|frame| h.app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let status_row = 8;
        assert!(matches!(buffer[(0, status_row)].bg, Color::Indexed(_)));
        for cell in &buffer.content {
            assert!(!matches!(cell.fg, Color::Rgb(..)), "{cell:?}");
            assert!(!matches!(cell.bg, Color::Rgb(..)), "{cell:?}");
        }
    }

    #[tokio::test]
    async fn the_loop_draws_until_told_to_stop() {
        let mut h = scripted(vec![]);
        for character in "/nope\r/quit\r".chars() {
            let code = match character {
                '\r' => KeyCode::Enter,
                other => KeyCode::Char(other),
            };
            h.sender.send(Event::Key(key(code))).unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();

        let exit = h.app.run(&mut terminal, &mut h.events).await;

        assert_eq!(exit, Ok(Exit::Quit));
        assert!(rows(&terminal).join("\n").contains("session first"));
    }

    #[tokio::test]
    async fn the_loop_quits_once_nothing_can_send_events() {
        let (_, mut events) = mpsc::unbounded_channel();
        let mut h = scripted(vec![]);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let exit = h.app.run(&mut terminal, &mut events).await;
        assert_eq!(exit, Ok(Exit::Quit));
    }

    /// Records a Session in `dir` whose id and first prompt are both `id`, last touched `age`
    /// seconds ago.
    fn summary(dir: &TempDir, id: &str, age: u64) -> SessionSummary {
        let path = path_of(dir, id);
        let mut log = SessionLog::create(&path).unwrap();
        let text = id.to_owned();
        log.append(&Entry::User { text: text.clone() }).unwrap();
        SessionSummary {
            path,
            first_prompt: Some(text),
            name: None,
            modified: SystemTime::now() - Duration::from_secs(age),
            parked: false,
        }
    }

    /// Starts an [`App`] with two stored Sessions as well as the new one.
    fn with_stored(provider: Arc<dyn Provider>) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let stored = vec![summary(&dir, "younger", 100), summary(&dir, "older", 200)];
        start(dir, provider, stored)
    }

    /// Returns the Session Names in the sidebar, top to bottom. Each must be short enough to leave
    /// two spaces before what its Session is doing.
    fn sidebar_order(h: &mut Harness) -> Vec<String> {
        let screen = h.screen_of(WIDE);
        let rows = screen.lines().skip(1);
        let names = rows.map(|row| row.chars().skip(3).collect::<String>());
        names
            .map(|row| row.split("  ").next().unwrap_or_default().to_owned())
            .take_while(|name| !name.is_empty())
            .collect()
    }

    #[tokio::test]
    async fn the_sidebar_lists_sessions_longest_wait_first() {
        let mut h = with_stored(Arc::new(NeverResponds));
        assert_eq!(sidebar_order(&mut h), ["older", "younger", "new session"]);
        assert_eq!(h.shown(), "first");
    }

    #[tokio::test]
    async fn the_sidebar_makes_way_on_a_narrow_screen() {
        let mut h = with_stored(Arc::new(NeverResponds));
        hiding_seconds(|| assert_snapshot!(h.drawn_at(WIDE)));
        assert_snapshot!(h.drawn_at(NARROW));
    }

    #[tokio::test]
    async fn keys_move_through_the_sidebar_and_wrap() {
        let mut h = with_stored(Arc::new(NeverResponds));
        h.press('e');
        assert_eq!(h.shown(), "older");
        h.press('e');
        assert_eq!(h.shown(), "younger");
        h.press('y');
        assert_eq!(h.shown(), "older");
        h.press('y');
        assert_eq!(h.shown(), "first");
    }

    #[tokio::test]
    async fn the_longest_wait_is_a_key_away_and_skips_the_session_on_screen() {
        let mut h = with_stored(Arc::new(NeverResponds));
        h.press('g');
        assert_eq!(h.shown(), "older");
        h.press('g');
        assert_eq!(h.shown(), "younger");
    }

    #[tokio::test]
    async fn with_nothing_else_waiting_the_longest_wait_key_stays_put() {
        let mut h = harness(Arc::new(NeverResponds));
        h.press('g');
        assert_eq!(h.shown(), "first");
    }

    #[tokio::test]
    async fn a_click_in_the_sidebar_shows_that_session() {
        let mut h = with_stored(Arc::new(NeverResponds));
        h.screen_of(WIDE);
        h.app.handle(Event::Click { column: 3, row: 2 });
        assert_eq!(h.shown(), "younger");
        h.app.handle(Event::Click { column: 90, row: 2 });
        assert_eq!(h.shown(), "younger");
    }

    #[tokio::test]
    async fn switching_to_a_stored_session_resumes_it() {
        let mut h = with_stored(Arc::new(NeverResponds));
        h.press('e');
        h.until(|event| matches!(event, Event::Report(SessionReport::Resumed(entries)) if !entries.is_empty()))
            .await;
        assert!(h.screen().contains("> older"));
    }

    #[tokio::test]
    async fn a_session_that_cannot_open_leaves_the_person_where_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let stored = vec![summary(&dir, "older", 100)];
        let mut h = start_with()
            .dir(dir)
            .provider(Arc::new(NeverResponds))
            .stored(stored)
            .adjust(|sessions| sessions.open_failure = Some("no keychain"))
            .call();

        h.press('e');
        assert_eq!(h.shown(), "first");
        assert!(
            h.screen()
                .contains("could not open the Session: no keychain")
        );

        h.enter("/new");
        assert_eq!(h.shown(), "first");
        assert!(h.screen().contains("could not start a new Session"));
    }

    #[tokio::test]
    async fn the_vim_command_switches_how_the_person_types_and_back() {
        let mut h = harness(Arc::new(NeverResponds));
        assert!(!h.screen().contains("INSERT"));
        h.enter("/vim");
        assert!(h.screen().contains("INSERT"));
        h.app.apply_cursor_style();
        assert_eq!(h.app.cursor_style, SetCursorStyle::SteadyBar);
        h.vim_reports(VimEvent::Submitted("/vim".into()));
        assert!(!h.screen().contains("INSERT"));
        assert!(!h.screen().contains("/vim\n"));
    }

    #[tokio::test]
    async fn a_session_starts_in_vim_if_the_person_wants_it() {
        let (sender, _events) = mpsc::unbounded_channel();
        let dir = tempfile::tempdir().unwrap();
        let mut wanted = opened(&path_of(&dir, "s"), Arc::new(NeverResponds), &sender);
        wanted.vim = true;
        let pane = Pane::opened(wanted);
        assert_eq!(pane.cursor_style(), SetCursorStyle::SteadyBar);
    }

    #[tokio::test]
    async fn the_vim_command_does_nothing_before_a_session_can_report() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = Pane::stored(summary(&dir, "stored", 1));
        pane.toggle_vim();
        assert_eq!(pane.cursor_style(), SetCursorStyle::DefaultUserShape);
    }

    #[tokio::test]
    async fn a_line_submitted_through_nvim_counts_as_entered() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter("/vim");
        assert_eq!(
            h.vim_reports(VimEvent::Submitted("/quit".into())),
            Some(Exit::Quit)
        );
    }

    #[tokio::test]
    async fn a_session_that_submits_while_away_comes_back_to_take_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let stored = vec![summary(&dir, "older", 100)];
        let mut h = start(dir, Arc::new(NeverResponds), stored);
        h.press('e');
        let away = h.shown();
        h.press('y');
        let event = Box::new(Event::Vim(VimEvent::Submitted("hello".into())));
        h.app.handle(Event::Session(away.clone(), event));
        assert_eq!(h.shown(), away);
        assert!(h.app.panes[h.app.shown].state().in_turn());
    }

    #[tokio::test]
    async fn losing_nvim_falls_back_to_the_prompt_with_the_text_so_far() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter("/vim");
        h.vim_reports(VimEvent::Lost {
            draft: Draft {
                text: "half typed".into(),
                cursor: 4,
            },
            failure: VimFailure::TimedOut,
        });
        assert_snapshot!(h.drawn_at(NARROW));
    }

    #[tokio::test]
    async fn a_report_from_an_nvim_already_left_behind_changes_nothing() {
        let mut h = harness(Arc::new(NeverResponds));
        h.app.handle(Event::Paste("kept".into()));
        h.vim_reports(VimEvent::Lost {
            draft: Draft::default(),
            failure: VimFailure::TimedOut,
        });
        assert!(h.screen().contains("> kept"));
    }

    #[tokio::test]
    async fn a_missing_nvim_is_mentioned_once() {
        let mut h = harness(Arc::new(NeverResponds));
        let lost = VimEvent::Lost {
            draft: Draft::default(),
            failure: VimFailure::NotInstalled,
        };
        h.vim_reports(lost.clone());
        h.vim_reports(lost);
        assert_eq!(h.screen().matches("nvim is not installed").count(), 1);
    }

    #[cfg(feature = "nvim-tests")]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_prompt_typed_into_a_real_nvim_starts_a_turn() {
        let mut h = scripted(vec![Ok(reply("done"))]);
        h.enter("/vim");
        for character in "hello".chars() {
            h.app.handle(Event::Key(key(KeyCode::Char(character))));
        }
        h.until(
            |event| matches!(event, Event::Vim(VimEvent::State(state)) if state.text == "hello"),
        )
        .await;
        let submit = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        h.app.handle(Event::Key(submit));
        h.until(|event| matches!(event, Event::Report(SessionReport::TurnEnded(_))))
            .await;
        assert!(h.screen().contains("done"));
    }

    #[tokio::test]
    async fn sessions_run_side_by_side() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter("one");
        h.enter("/new");
        assert_eq!(h.shown(), "new1");
        assert!(h.screen().contains("ready"));
        h.enter("two");

        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        assert_eq!(h.app.panes[1].state(), SessionState::Working);
        assert_eq!(sidebar_order(&mut h), ["one", "two"]);
    }

    #[tokio::test]
    async fn input_stays_with_its_session() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter_new();
        h.app.handle(Event::Paste("half typed".into()));
        h.press('e');
        assert_eq!(h.shown(), "first");
        assert!(!h.screen().contains("half typed"));
        h.press('e');
        assert!(h.screen().contains("half typed"));
    }

    #[tokio::test]
    async fn ctrl_c_refuses_to_quit_over_work_on_another_session() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter("one");
        h.enter("/new");

        assert_eq!(h.press('c'), None);
        assert!(h.screen().contains("other Sessions are working"));
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
    }

    #[tokio::test]
    async fn turns_end_unseen_until_the_session_is_viewed() {
        let mut h = scripted(vec![Ok(reply("all done"))]);
        h.enter("one");
        h.enter_new();
        h.until_turn_ends().await;
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("+ one"), "{screen}");

        h.press('e');
        assert_eq!(h.shown(), "first");
        let screen = h.screen_of(WIDE);
        assert!(!screen.contains("+ one"), "{screen}");
    }

    #[tokio::test]
    async fn a_failed_turn_on_another_session_is_marked() {
        let provider = ScriptedProvider::new(vec![Err("down".to_owned())]);
        let mut h = harness(Arc::new(provider));
        h.enter("one");
        h.enter("/new");
        h.until_turn_ends().await;
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("x one"), "{screen}");
    }

    #[tokio::test]
    async fn questions_wait_with_their_session_and_never_catch_stray_keys() {
        let script = vec![Ok(bash_call("touch ran")), Ok(reply("all done"))];
        let mut h = scripted(script);
        h.enter("go");
        h.enter_new();
        h.until(|event| matches!(event, Event::Report(SessionReport::Approval(_))))
            .await;

        hiding_seconds(|| assert_snapshot!(h.drawn_at(WIDE)));

        h.app.handle(Event::Key(key(KeyCode::Char('y'))));
        assert!(!h.dir.path().join("ran").exists());

        h.press('g');
        assert!(h.screen().contains("[y/N]"));
        h.app.handle(Event::Key(key(KeyCode::Char('y'))));
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_turn_ends().await;
        assert!(h.dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn the_keys_for_getting_away_work_with_a_question_open() {
        let mut h = scripted(vec![Ok(bash_call("touch ran"))]);
        h.enter("/new");
        h.press('e');
        h.enter("go");
        h.until(|event| matches!(event, Event::Report(SessionReport::Approval(_))))
            .await;
        h.press('e');
        assert_eq!(h.shown(), "new1");
    }

    #[tokio::test]
    async fn a_narrow_status_line_says_what_the_sidebar_would() {
        let mut h = scripted(vec![Ok(bash_call("touch ran"))]);
        h.enter("go");
        h.enter_new();
        h.until(|event| matches!(event, Event::Report(SessionReport::Approval(_))))
            .await;
        assert_snapshot!(h.drawn_at(NARROW));
        hiding_seconds(|| assert_snapshot!(h.drawn_at(WIDE)));
    }

    #[tokio::test]
    async fn events_from_an_unknown_session_are_ignored() {
        let mut h = harness(Arc::new(NeverResponds));
        let gone = Event::Session("gone".into(), Box::new(Event::Resize));
        assert_eq!(h.app.handle(gone), None);
        assert_eq!(h.app.handle(archived("gone", Err("no such Session"))), None);
        assert!(!h.screen().contains("not archived"));
    }

    #[test]
    fn only_control_keys_are_global() {
        assert_eq!(global_action(alt('e')), Some(Action::NextSession));
        assert_eq!(global_action(alt('y')), Some(Action::PreviousSession));
        assert_eq!(global_action(alt('n')), None);
        assert_eq!(global_action(alt('p')), None);
        assert_eq!(global_action(key(KeyCode::Char('e'))), None);
        assert_eq!(global_action(alt('z')), None);
        assert_eq!(global_action(ctrl('n')), None);
        assert_eq!(global_action(key(KeyCode::Enter)), None);
    }

    #[tokio::test]
    async fn stored_session_files_are_left_alone_until_opened() {
        let dir = tempfile::tempdir().unwrap();
        let stored = vec![summary(&dir, "older", 100)];
        let before = fs::read(path_of(&dir, "older")).unwrap();
        let mut h = start(dir, Arc::new(NeverResponds), stored);
        h.screen_of(WIDE);
        assert_eq!(fs::read(path_of(&h.dir, "older")).unwrap(), before);
    }

    /// Returns the name the person gave the Session with id `id`, as stored in `dir`.
    fn stored_name(dir: &TempDir, id: &str) -> Option<String> {
        let listed = session::list_sessions(dir.path()).unwrap();
        let found = listed
            .into_iter()
            .find(|summary| summary.path == path_of(dir, id));
        found.unwrap().name
    }

    #[tokio::test]
    async fn rename_lists_the_session_by_its_new_name_until_it_is_taken_away() {
        let mut h = with_stored(Arc::new(NeverResponds));

        assert_eq!(h.enter("/rename fix the sidebar"), None);
        assert_eq!(
            sidebar_order(&mut h),
            ["older", "younger", "fix the sidebar"]
        );
        assert_eq!(
            stored_name(&h.dir, "first").as_deref(),
            Some("fix the sidebar")
        );

        h.enter("/rename");
        assert_eq!(sidebar_order(&mut h), ["older", "younger", "new session"]);
        assert_eq!(stored_name(&h.dir, "first"), None);
    }

    #[tokio::test]
    async fn the_connection_command_switches_the_session_on_screen_or_offers_a_choice() {
        let mut h = harness(Arc::new(NeverResponds));
        assert!(h.screen_of(WIDE).contains("| anthropic |"));

        h.enter("/connection openai");
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("| openai |"), "{screen}");
        assert!(
            screen.contains("runs on openai from the next Turn"),
            "{screen}"
        );
        h.enter("/connection nope");
        let screen = h.screen_of(WIDE);
        assert!(
            screen.contains("not switched: no connection named nope"),
            "{screen}"
        );

        h.enter("/connection");
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("> 3. openai (now)"), "{screen}");
        h.app.handle(Event::Key(key(KeyCode::Up)));
        h.app.handle(Event::Key(key(KeyCode::Up)));
        h.app.handle(Event::Key(key(KeyCode::Enter)));
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("| anthropic |"), "{screen}");
        assert!(!screen.contains("(now)"), "{screen}");
    }

    #[tokio::test]
    async fn a_name_outlasts_the_first_prompt_and_gives_way_back_to_it() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter("/rename planning");
        h.enter("hello");
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("planning"), "{screen}");
        assert!(!screen.contains("hello  "), "{screen}");

        h.enter("/rename");
        assert!(h.screen_of(WIDE).contains("* hello"));
    }

    #[tokio::test]
    async fn a_stored_session_is_listed_by_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut named = summary(&dir, "older", 100);
        named.name = Some("named".into());
        let mut h = start(dir, Arc::new(NeverResponds), vec![named]);
        assert_eq!(sidebar_order(&mut h), ["named", "new session"]);
    }

    #[tokio::test]
    async fn a_name_that_cannot_be_stored_changes_nothing() {
        let mut h = start_with()
            .dir(tempfile::tempdir().unwrap())
            .provider(Arc::new(NeverResponds))
            .adjust(|sessions| sessions.rename_failure = Some("disk full"))
            .call();

        h.enter("/rename lost");

        assert_eq!(sidebar_order(&mut h), ["new session"]);
        assert!(h.screen().contains("not renamed: disk full"));
    }

    #[tokio::test]
    async fn new_sessions_get_a_worktree_unless_the_main_checkout_is_asked_for() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter_new();
        h.enter("/new-main");
        assert_eq!(*h.places.borrow(), [Place::Worktree, Place::MainCheckout]);
    }

    #[tokio::test]
    async fn archiving_puts_the_session_away_and_moves_on_to_the_queue() {
        let mut h = with_stored(Arc::new(NeverResponds));
        h.enter("/archive");
        assert_eq!(*h.archived.borrow(), ["first"]);
        assert_eq!(h.shown(), "older");
        assert_eq!(sidebar_order(&mut h), ["older", "younger"]);
    }

    #[tokio::test]
    async fn ctrl_x_archives_too() {
        let mut h = with_stored(Arc::new(NeverResponds));
        h.press('x');
        h.press('x');
        assert_eq!(*h.archived.borrow(), ["first", "older"]);
        assert_eq!(h.shown(), "younger");
    }

    #[tokio::test]
    async fn archiving_the_last_session_starts_a_new_one() {
        let mut h = harness(Arc::new(NeverResponds));
        h.press('x');
        assert_eq!(*h.archived.borrow(), ["first"]);
        assert_eq!(h.shown(), "new1");
        assert_eq!(h.app.panes.len(), 1);
    }

    #[tokio::test]
    async fn a_session_with_a_turn_running_cannot_be_archived() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter("go");
        h.press('x');
        assert_eq!(*h.archived.borrow(), Vec::<String>::new());
        assert!(h.screen().contains("a Turn is running"));
    }

    #[tokio::test]
    async fn work_that_would_be_lost_stops_archiving_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = start_with()
            .dir(dir)
            .provider(Arc::new(NeverResponds))
            .adjust(|sessions| sessions.refusal = Some("it has 2 unlanded commits"))
            .call();

        h.enter("/archive");
        assert_eq!(*h.archived.borrow(), Vec::<String>::new());
        assert_eq!(h.shown(), "first");
        assert!(h.screen().contains("2 unlanded commits"));

        h.enter("/archive --force");
        assert_eq!(*h.archived.borrow(), ["first"]);
    }

    /// Returns the report that putting away the Session with id `id` ended in `outcome`.
    fn archived(id: &str, outcome: Result<(), &str>) -> Event {
        Event::Archived {
            id: id.to_owned(),
            outcome: outcome.map_err(str::to_owned),
        }
    }

    /// Starts an [`App`] with one stored Session as well as the new one, whose archives stay
    /// unfinished until the test reports how they went.
    fn holding_archives() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let stored = vec![summary(&dir, "older", 100)];
        start_with()
            .dir(dir)
            .provider(Arc::new(NeverResponds))
            .stored(stored)
            .adjust(|sessions| sessions.hold_archives = true)
            .call()
    }

    #[tokio::test]
    async fn an_archive_leaves_the_list_before_it_finishes() {
        let mut h = holding_archives();

        h.press('x');

        assert_eq!(*h.archived.borrow(), ["first"]);
        assert_eq!(h.shown(), "older");
        assert_eq!(sidebar_order(&mut h), ["older"]);
        assert_eq!(h.app.handle(archived("first", Ok(()))), None);
        assert_eq!(sidebar_order(&mut h), ["older"]);
    }

    #[tokio::test]
    async fn a_failed_archive_lists_the_session_again() {
        let mut h = holding_archives();
        h.press('x');

        assert_eq!(h.app.handle(archived("first", Err("git said no"))), None);

        assert_eq!(h.shown(), "older");
        assert_eq!(sidebar_order(&mut h), ["older", "new session"]);
        assert!(h.screen().contains("not archived: git said no"));
    }

    #[tokio::test]
    async fn the_session_on_screen_survives_removing_one_stored_before_it() {
        let mut h = with_stored(Arc::new(NeverResponds));
        h.press('y');
        assert_eq!(h.shown(), "younger");
        h.press('x');
        assert_eq!(h.shown(), "older");
        assert_eq!(*h.archived.borrow(), ["younger"]);
        assert_eq!(sidebar_order(&mut h), ["older", "new session"]);
    }

    #[tokio::test]
    async fn being_unable_to_move_on_keeps_the_session_on_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = start_with()
            .dir(dir)
            .provider(Arc::new(NeverResponds))
            .adjust(|sessions| sessions.open_failure = Some("no keychain"))
            .call();
        h.press('x');
        assert_eq!(h.shown(), "first");
        assert_eq!(*h.archived.borrow(), Vec::<String>::new());
    }

    /// The new binary a test build produces.
    fn binary() -> PathBuf {
        PathBuf::from("/bin/sstack")
    }

    fn built() -> Event {
        Event::Report(SessionReport::BuildFinished(Ok(binary())))
    }

    fn two_steps() -> Harness {
        scripted(vec![Ok(bash_call("touch ran")), Ok(reply("all done"))])
    }

    #[tokio::test]
    async fn a_rebuild_with_nothing_running_relaunches_once_built() {
        let mut h = scripted(vec![]);
        assert_eq!(h.enter("/rebuild"), None);
        assert_eq!(*h.builds.borrow(), ["started"]);
        assert_eq!(h.app.handle(built()), Some(Exit::Rebuild(binary())));
    }

    #[tokio::test]
    async fn a_rebuild_of_the_worktree_builds_the_worktree_of_the_session_on_screen() {
        let mut h = scripted(vec![]);
        assert_eq!(h.enter("/rebuild --worktree"), None);
        assert_eq!(*h.builds.borrow(), ["started the worktree of first"]);
        assert_eq!(h.app.handle(built()), Some(Exit::Rebuild(binary())));
    }

    #[tokio::test]
    async fn a_rebuild_waits_for_both_the_build_and_the_step_in_progress() {
        let (mut h, gate) = gated(vec![Ok(read_call()), Ok(reply("all done"))]);
        assert_eq!(h.drain_with_a_step_running("/rebuild").await, None);
        assert_eq!(h.app.handle(built()), None);
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("built, 1 running"), "{screen}");

        gate.add_permits(1);

        assert_eq!(h.until_exit().await, Exit::Rebuild(binary()));
        let entries = session::lock_free_read(&path_of(&h.dir, "first")).unwrap();
        assert_eq!(entries.last(), Some(&Entry::TurnParked));
        assert!(matches!(
            entries[entries.len() - 2],
            Entry::ToolResult { .. }
        ));
    }

    #[tokio::test]
    async fn a_rebuild_puts_off_an_open_question_rather_than_wait_for_it() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Parked);
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("- go"), "{screen}");
        assert!(screen.contains("parked"), "{screen}");

        assert_eq!(h.app.handle(built()), Some(Exit::Rebuild(binary())));
        assert!(matches!(h.session().last(), Some(Entry::TurnPutOff { .. })));
        assert!(!h.dir.path().join("ran").exists());
    }

    /// Runs a Turn in the first Session of `h` until it asks, and returns the screen as the
    /// person sees it then. Then rebuilds from a new Session, relaunches into the first Session
    /// with a model that gives `script`, and carries its Turn on until it asks again.
    async fn asked_across_a_relaunch(mut h: Harness, script: Vec<Scripted>) -> (String, Harness) {
        h.enter("go");
        h.enter_new();
        h.until(|event| {
            matches!(
                event,
                Event::Report(SessionReport::Approval(_) | SessionReport::Question(_))
            )
        })
        .await;
        h.press('g');
        let before = h.screen();
        h.press('e');
        h.enter("/rebuild");
        h.until_turn_ends().await;
        h.app.handle(built());
        let provider = Arc::new(ScriptedProvider::new(script));
        let stored = session::list_sessions(h.dir.path()).unwrap();
        let stored = stored
            .into_iter()
            .filter(|summary| summary.parked)
            .collect();
        let mut h = start(h.stopped().await, provider, stored);
        h.app.continue_parked();
        h.until(|event| {
            matches!(
                event,
                Event::Report(SessionReport::Approval(_) | SessionReport::Question(_))
            )
        })
        .await;
        (before, h)
    }

    #[tokio::test]
    async fn after_a_relaunch_a_put_off_question_is_shown_as_it_was() {
        let (before, mut h) = asked_across_a_relaunch(two_steps(), vec![Ok(reply("done"))]).await;

        assert_eq!(h.screen(), before);
        h.approve();
        h.until_turn_ends().await;
        assert!(h.dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn after_a_relaunch_the_models_questions_are_shown_as_they_were() {
        let script = vec![Ok(ask_call("Which store?", &["Files", "SQLite"]))];
        let (before, mut h) = asked_across_a_relaunch(scripted(script), vec![]).await;

        assert_eq!(h.screen(), before);
        assert_eq!(h.app.panes[0].state(), SessionState::AwaitingAnswer);
    }

    #[tokio::test]
    async fn parked_turns_carry_on_after_a_relaunch_and_ask_again_what_was_put_off() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
        h.app.handle(built());
        h.until_exit().await;

        let stored = SessionSummary {
            path: path_of(&h.dir, "first"),
            first_prompt: Some("go".into()),
            name: None,
            modified: SystemTime::now(),
            parked: true,
        };
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(reply("carried on"))]));
        let mut h = start(h.stopped().await, provider, vec![stored]);
        assert_eq!(h.app.panes[0].state(), SessionState::Parked);

        h.app.continue_parked();

        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_asked_then_approve().await;
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
        assert!(h.dir.path().join("ran").exists());
        assert!(h.screen().contains("carried on"));
        assert_eq!(h.session().len(), 5);
    }

    #[tokio::test]
    async fn a_parked_session_that_cannot_open_stays_parked_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut stored = summary(&dir, "older", 100);
        stored.parked = true;
        let mut h = start_with()
            .dir(dir)
            .provider(Arc::new(NeverResponds))
            .stored(vec![stored])
            .adjust(|sessions| sessions.open_failure = Some("no keychain"))
            .call();

        h.app.continue_parked();

        assert_eq!(h.app.panes[0].state(), SessionState::Parked);
        assert!(h.screen().contains("could not carry on a Session"));
    }

    #[tokio::test]
    async fn a_failed_build_stops_the_drain_and_the_turns_carry_on() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
        h.until_turn_ends().await;

        let failed = Event::Report(SessionReport::BuildFinished(Err(
            "the build did not succeed".into(),
        )));
        assert_eq!(h.app.handle(failed), None);

        assert!(!h.app.harness.phase().is_draining());
        assert!(h.screen().contains("the build did not succeed"));
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_asked_then_approve().await;
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
        assert!(h.screen().contains("all done"));
    }

    #[tokio::test]
    async fn alt_c_backs_out_of_a_rebuild_without_cancelling_any_turn() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;

        assert_eq!(h.app.handle(Event::Key(alt('c'))), None);

        assert!(!h.app.harness.phase().is_draining());
        assert_eq!(*h.builds.borrow(), ["started", "cancelled"]);
        // The question was put off before the back-out, so the Turn parks and carries on at once.
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_asked_then_approve().await;
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
        assert!(h.dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn a_turn_that_parks_just_after_a_drain_is_abandoned_carries_on() {
        let (mut h, gate) = gated(vec![Ok(read_call()), Ok(reply("all done"))]);
        h.drain_with_a_step_running("/rebuild").await;
        h.app.handle(Event::Key(alt('c')));
        h.app.panes[0].set_paused(true);
        gate.add_permits(1);
        h.until(|event| matches!(event, Event::Report(SessionReport::TurnEnded(_))))
            .await;
        h.app.panes[0].set_paused(false);
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        gate.add_permits(1);
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
    }

    #[tokio::test]
    async fn nothing_new_starts_during_a_drain_but_the_person_can_still_move_around() {
        let (mut h, _gate) = gated(vec![]);
        h.drain_with_a_step_running("/rebuild").await;

        h.enter("hello");
        assert!(h.screen().contains("the harness is stopping"));
        assert_eq!(h.app.panes[1].state(), SessionState::Idle);
        h.enter("/new");
        h.enter("/rebuild");
        h.enter("/quit");
        h.press('x');
        assert_eq!(h.app.panes.len(), 2);
        assert_eq!(*h.builds.borrow(), ["started"]);

        h.press('e');
        assert_eq!(h.shown(), "first");
    }

    #[tokio::test]
    async fn the_narrow_status_line_says_what_a_drain_is_doing() {
        let (mut h, _gate) = gated(vec![]);
        h.drain_with_a_step_running("/rebuild").await;
        let narrow = h.screen();
        assert!(narrow.contains("building, 1 running"), "{narrow}");
    }

    #[tokio::test]
    async fn quitting_waits_for_turns_to_reach_the_end_of_their_step() {
        let mut h = two_steps();
        assert_eq!(h.drain_with_a_question_open("/quit").await, None);
        assert!(h.screen_of(WIDE).contains("quitting, 1 running"));
        assert_eq!(*h.builds.borrow(), Vec::<&str>::new());

        h.approve();

        assert_eq!(h.until_exit().await, Exit::Quit);
        let entries = session::lock_free_read(&path_of(&h.dir, "first")).unwrap();
        assert_eq!(entries.last(), Some(&Entry::TurnParked));
    }

    #[tokio::test]
    async fn quitting_a_second_time_does_not_wait() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/quit").await;
        assert_eq!(h.enter("/quit"), Some(Exit::Quit));
    }

    #[tokio::test]
    async fn alt_c_backs_out_of_quitting() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/quit").await;
        assert_eq!(h.app.handle(Event::Key(alt('c'))), None);
        assert!(!h.app.harness.phase().is_draining());
    }

    #[tokio::test]
    async fn a_failed_relaunch_says_so_and_turns_carry_on_here() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
        h.app.handle(built());
        assert_eq!(h.until_exit().await, Exit::Rebuild(binary()));

        h.app.relaunch_failed("exec failed");

        assert!(h.screen().contains("rebuild failed: exec failed"));
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_asked_then_approve().await;
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
    }

    #[test]
    fn the_person_is_told_what_a_drain_is_waiting_on() {
        assert_eq!(progress(&Phase::Running, 2), None);
        let quitting = progress(&Phase::DrainingToQuit, 2).unwrap();
        assert_eq!(quitting, "quitting, 2 running, Alt-C stops");
        let building = progress(&Phase::DrainingToRebuild { binary: None }, 1).unwrap();
        assert!(building.starts_with("building, 1 running"), "{building}");
        let binary = Some(PathBuf::from("/b"));
        let built = progress(&Phase::DrainingToRebuild { binary }, 0).unwrap();
        assert!(built.starts_with("built, 0 running"), "{built}");
    }
}
