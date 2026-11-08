//! The interactive harness as a whole: it keeps every Session, routes events to them and carries
//! out what they ask for.

use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use bon::bon;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::{Frame, Terminal};
use sstack::skills::{self, Lookup};
use sstack::vim::{VimEvent, VimFailure};
use sstack::workspace::SessionSummary;

use super::component::{Component, KeyOutcome};
use super::components::{Activity, Row, SIDEBAR_WIDTH, Sidebar};
use super::event::{Action, Event, EventReceiver};
use super::lifecycle::{self, Effect, Exit, Interrupt, Phase, SessionState, Trigger};
use super::pane::{Opened, Pane};
use super::theme::ColorDepth;
use crate::command::{Command, parse_command};
use crate::setup::{Place, Start};

/// The reason given to the person when their input is not acted on.
const BUSY: &str = "a Turn is running: wait for it to end, or press Alt-C to cancel it";
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

    /// Puts the Session with id `id` away, deleting its worktree and any work there.
    ///
    /// # Errors
    /// Returns an error if the Session could not be put away.
    fn archive(&mut self, id: &str) -> Result<()>;

    /// Stores `name` as the Session Name of the Session with id `id`, so that it lasts beyond this
    /// run. With `None`, the Session is known by its first prompt again.
    ///
    /// # Errors
    /// Returns an error if the name could not be stored.
    fn rename(&mut self, id: &str, name: Option<&str>) -> Result<()>;

    /// Starts building the Harness Source in the background, which ends in an
    /// [`Event::BuildFinished`].
    fn start_build(&mut self);

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
    /// Whether the harness is carrying on or on its way out.
    phase: Phase,
    /// The list of Sessions.
    sidebar: Sidebar,
    /// Where Sessions are opened and archived as the person asks.
    sessions: Box<dyn Sessions + 'a>,
    /// How many colors the terminal can show, which the theme's colors are fitted to.
    colors: ColorDepth,
    /// The cursor shape the terminal was last told to use.
    cursor_style: SetCursorStyle,
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
            phase: Phase::Running,
            sidebar: Sidebar::default(),
            sessions,
            colors,
            cursor_style: SetCursorStyle::DefaultUserShape,
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
            Event::BuildFinished(outcome) => return self.build_finished(outcome),
            Event::Click { column, row } => self.click(column, row),
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
            Event::TurnEnded(_) if !self.phase.is_draining() => {
                self.panes[index].continue_turn();
            }
            _ => {}
        }
        self.check_settled()
    }

    /// Reports that no Turn is running, if that is news the current phase is waiting for.
    fn check_settled(&mut self) -> Option<Exit> {
        let settled = !self.panes.iter().any(|pane| pane.state().in_turn());
        (self.phase.is_draining() && settled)
            .then(|| self.advance(Trigger::Settled))
            .flatten()
    }

    /// Moves the harness to its next phase and does what that takes.
    fn advance(&mut self, trigger: Trigger) -> Option<Exit> {
        let (phase, effect) = mem::take(&mut self.phase).after(trigger);
        self.phase = phase;
        match effect {
            Effect::None => return None,
            Effect::Drain => self.set_paused(true),
            Effect::DrainAndBuild => {
                self.set_paused(true);
                self.sessions.start_build();
            }
            Effect::Abandon => {
                self.sessions.cancel_build();
                self.resume_parked();
            }
            Effect::Exit(exit) => return Some(exit),
        }
        self.check_settled()
    }

    /// Asks every Session's Turns to stop at the end of their Step, or stops asking.
    fn set_paused(&self, paused: bool) {
        for pane in &self.panes {
            pane.set_paused(paused);
        }
    }

    /// Lets every Session carry on, including any Turn that has already parked.
    fn resume_parked(&mut self) {
        self.set_paused(false);
        for pane in &mut self.panes {
            pane.continue_turn();
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
        self.advance(Trigger::BuildFinished(binary))
            .or_else(|| self.check_settled())
    }

    /// Tells the person that the relaunch failed, and lets every Session carry on here.
    pub fn relaunch_failed(&mut self, failure: &str) {
        self.notice(format!("rebuild failed: {failure}"));
        self.resume_parked();
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
        self.panes[index].update(&Event::Notice(notice));
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
            Action::Interrupt => return self.interrupt(),
            Action::ApprovalAnswered => self.panes[self.shown].answered(),
            Action::NextSession => self.step(1),
            Action::PreviousSession => self.step(-1),
            Action::LongestWaiting => self.show_longest_waiting(),
            Action::Archive => self.archive(false),
        }
        None
    }

    /// Acts on Alt-C, which backs out of a drain before it does anything else.
    fn interrupt(&mut self) -> Option<Exit> {
        if self.phase.is_draining() {
            return self.advance(Trigger::Interrupted);
        }
        let pane = &self.panes[self.shown];
        match lifecycle::interrupt(pane.state(), self.others_in_turn()) {
            Interrupt::CancelTurn => pane.cancel(),
            Interrupt::Quit => return Some(Exit::Quit),
            Interrupt::RefuseToQuit => self.notice(OTHERS_WORKING.to_owned()),
        }
        None
    }

    /// Returns whether any Session but the one on screen has a Turn running.
    fn others_in_turn(&self) -> bool {
        let others = self.panes.iter().enumerate();
        others
            .filter(|(index, _)| *index != self.shown)
            .any(|(_, pane)| pane.state().in_turn())
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
        let command = parse_command(line);
        if self.phase.is_draining() && command.changes_work() {
            self.notice(DRAINING.to_owned());
            return None;
        }
        match command {
            Command::Prompt(text) => self.prompt(text),
            Command::Rebuild => return self.advance(Trigger::RebuildAsked),
            Command::New => self.open_new(Place::Worktree),
            Command::NewMain => self.open_new(Place::MainCheckout),
            Command::Archive { force } => self.archive(force),
            Command::Vim => self.panes[self.shown].toggle_vim(),
            Command::Rename(name) => self.rename(name),
            Command::Quit => return self.quit(),
            Command::Unknown(line) => self.run_skill(line),
            Command::Empty => {}
        }
        None
    }

    /// Starts a Turn on the Skill that the slash-prefixed `line` names, with the rest of the line
    /// added after its instructions, or tells the person why it cannot.
    fn run_skill(&mut self, line: &str) {
        match self.skill_prompt(line) {
            Ok(_) if self.phase.is_draining() => self.notice(DRAINING.to_owned()),
            Ok(text) => self.prompt(&text),
            Err(notice) => self.notice(notice),
        }
    }

    /// Returns the prompt that runs the Skill the slash-prefixed `line` names in the Session on
    /// screen, or what to tell the person instead.
    fn skill_prompt(&self, line: &str) -> Result<String, String> {
        let request = line.trim_start_matches('/');
        let (name, added) = request
            .split_once(char::is_whitespace)
            .unwrap_or((request, ""));
        match skills::find(self.panes[self.shown].skills(), name) {
            Lookup::Found(skill) => {
                let loaded = skills::load(skill)
                    .map_err(|error| format!("could not load the Skill {name}: {error}"))?;
                Ok(format!("{loaded}\n\n{}", added.trim())
                    .trim_end()
                    .to_owned())
            }
            Lookup::Ambiguous(names) => Err(format!(
                "/{name} could mean {}: type the full name",
                names.join(" or ")
            )),
            Lookup::Missing => Err(format!("unknown command: {line}")),
        }
    }

    /// Starts to quit, or stops waiting if already quitting. A Rebuild has to be backed out of
    /// first, because it would otherwise end in a relaunch.
    fn quit(&mut self) -> Option<Exit> {
        if self.phase.is_rebuilding() {
            self.notice(DRAINING.to_owned());
            return None;
        }
        self.advance(Trigger::QuitAsked)
    }

    /// Starts a Turn on `text` in the Session on screen, if it is free to run one.
    fn prompt(&mut self, text: &str) {
        if self.panes[self.shown].state().in_turn() {
            self.notice(BUSY.to_owned());
        } else {
            self.panes[self.shown].start_turn(text);
        }
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
        if self.phase.is_draining() {
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
            self.finish_archiving(leaving, &id);
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

    /// Puts away the Session at position `leaving`, whose id is `id`, now that it is off screen.
    fn finish_archiving(&mut self, leaving: usize, id: &str) {
        if let Err(error) = self.sessions.archive(id) {
            self.notice(format!("not archived: {error}"));
            return;
        }
        self.panes.remove(leaving);
        if self.shown > leaving {
            self.shown -= 1;
        }
    }

    /// Tells the person something from the harness itself, in the Session on screen.
    pub fn notice(&mut self, text: String) {
        self.panes[self.shown].update(&Event::Notice(text));
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
        let progress = self.phase.progress(running.count());
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
    use crate::tui::test_support::{alt, bash_call, ctrl, drawn, key, opened, reply, rows};
    use anyhow::{anyhow, bail};
    use async_trait::async_trait;
    use insta::assert_snapshot;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use sstack::provider::scripted::{Scripted, ScriptedProvider};
    use sstack::provider::{ModelRequest, Provider, ProviderError, StreamEvent};
    use sstack::session::{AssistantEntry, Entry, SessionLog};
    use sstack::vim::Draft;
    use sstack::workspace;
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

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
        builds: Rc<RefCell<Vec<&'static str>>>,
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
        builds: Rc<RefCell<Vec<&'static str>>>,
        /// Why opening fails, if it does.
        open_failure: Option<&'static str>,
        /// Why archiving is refused, if it is.
        refusal: Option<&'static str>,
        /// Why archiving fails, if it does.
        archive_failure: Option<&'static str>,
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

        fn archive(&mut self, id: &str) -> Result<()> {
            if let Some(failure) = self.archive_failure {
                bail!(failure);
            }
            self.archived.borrow_mut().push(id.to_owned());
            Ok(())
        }

        fn rename(&mut self, id: &str, name: Option<&str>) -> Result<()> {
            if let Some(failure) = self.rename_failure {
                bail!(failure);
            }
            Ok(workspace::name_session(&self.root, id, name)?)
        }

        fn start_build(&mut self) {
            self.builds.borrow_mut().push("started");
        }

        fn cancel_build(&mut self) {
            self.builds.borrow_mut().push("cancelled");
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
            archive_failure: None,
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
        fn rebuilt(self) -> Harness {
            start(self.dir, Arc::new(ScriptedProvider::new(vec![])), vec![])
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
            self.until(|event| matches!(event, Event::Approval(_)))
                .await;
            exit
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
            self.until(|event| matches!(event, Event::TurnEnded(_)))
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
            SessionLog::open(&path_of(&self.dir, "first")).unwrap().1
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

        let mut h = h.rebuilt();
        h.until(|event| matches!(event, Event::Resumed(_))).await;

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
        h.until(|event| matches!(event, Event::Approval(_))).await;
        assert_snapshot!(h.drawn_at(NARROW));

        assert_eq!(h.app.handle(Event::Key(key(KeyCode::Char('y')))), None);
        h.until_turn_ends().await;

        assert!(h.dir.path().join("ran").exists());
        assert_snapshot!(h.drawn_at(NARROW));
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

    #[tokio::test]
    async fn while_a_turn_runs_ctrl_c_cancels_it_and_new_turns_wait() {
        let mut h = harness(Arc::new(NeverResponds));
        h.enter("go");
        h.until(|event| matches!(event, Event::TurnStarted(_)))
            .await;
        assert!(h.screen().contains("working"));

        assert_eq!(h.enter("again"), None);
        assert!(h.screen().contains("a Turn is running"));

        assert_eq!(h.app.handle(Event::Key(alt('c'))), None);
        h.until_turn_ends().await;
        assert!(h.screen().contains("[cancelled]"));
        assert_eq!(h.session().last(), Some(&Entry::TurnCancelled));

        assert_eq!(h.app.handle(Event::Key(alt('c'))), Some(Exit::Quit));
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
        h.until(|event| matches!(event, Event::Resumed(entries) if !entries.is_empty()))
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
        h.until(|event| matches!(event, Event::TurnEnded(_))).await;
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
        h.until(|event| matches!(event, Event::Approval(_))).await;

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
        h.until(|event| matches!(event, Event::Approval(_))).await;
        h.press('e');
        assert_eq!(h.shown(), "new1");
    }

    #[tokio::test]
    async fn a_narrow_status_line_says_what_the_sidebar_would() {
        let mut h = scripted(vec![Ok(bash_call("touch ran"))]);
        h.enter("go");
        h.enter_new();
        h.until(|event| matches!(event, Event::Approval(_))).await;
        assert_snapshot!(h.drawn_at(NARROW));
        hiding_seconds(|| assert_snapshot!(h.drawn_at(WIDE)));
    }

    #[tokio::test]
    async fn events_from_an_unknown_session_are_ignored() {
        let mut h = harness(Arc::new(NeverResponds));
        let gone = Event::Session("gone".into(), Box::new(Event::Resize));
        assert_eq!(h.app.handle(gone), None);
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
        let listed = workspace::list_sessions(dir.path()).unwrap();
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

    #[tokio::test]
    async fn a_failed_archive_keeps_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let stored = vec![summary(&dir, "older", 100)];
        let mut h = start_with()
            .dir(dir)
            .provider(Arc::new(NeverResponds))
            .stored(stored)
            .adjust(|sessions| sessions.archive_failure = Some("git said no"))
            .call();

        h.press('x');

        assert_eq!(h.app.panes.len(), 2);
        assert_eq!(h.shown(), "older");
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
        Event::BuildFinished(Ok(binary()))
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
    async fn a_rebuild_waits_for_both_the_build_and_the_step_in_progress() {
        let mut h = two_steps();
        assert_eq!(h.drain_with_a_question_open("/rebuild").await, None);
        assert_eq!(h.app.handle(built()), None);
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("built, 1 running"), "{screen}");
        assert!(screen.contains("! go"), "{screen}");

        h.approve();

        assert_eq!(h.until_exit().await, Exit::Rebuild(binary()));
        let entries = SessionLog::open(&path_of(&h.dir, "first")).unwrap().1;
        assert_eq!(entries.last(), Some(&Entry::TurnParked));
        assert!(h.dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn a_build_that_finishes_after_the_turns_stop_still_relaunches() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
        h.approve();
        h.until(|event| matches!(event, Event::TurnEnded(_))).await;
        assert_eq!(h.app.panes[0].state(), SessionState::Parked);
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("- go"), "{screen}");
        assert!(screen.contains("parked"), "{screen}");

        assert_eq!(h.app.handle(built()), Some(Exit::Rebuild(binary())));
    }

    #[tokio::test]
    async fn parked_turns_carry_on_after_a_relaunch() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
        h.app.handle(built());
        h.approve();
        h.until_exit().await;

        let stored = SessionSummary {
            path: path_of(&h.dir, "first"),
            first_prompt: Some("go".into()),
            name: None,
            modified: SystemTime::now(),
            parked: true,
        };
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(reply("carried on"))]));
        let mut h = start(h.dir, provider, vec![stored]);
        assert_eq!(h.app.panes[0].state(), SessionState::Parked);

        h.app.continue_parked();

        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
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
        h.approve();
        h.until(|event| matches!(event, Event::TurnEnded(_))).await;

        let failed = Event::BuildFinished(Err("the build did not succeed".into()));
        assert_eq!(h.app.handle(failed), None);

        assert!(!h.app.phase.is_draining());
        assert!(h.screen().contains("the build did not succeed"));
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
        assert!(h.screen().contains("all done"));
    }

    #[tokio::test]
    async fn alt_c_backs_out_of_a_rebuild_without_cancelling_any_turn() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;

        assert_eq!(h.app.handle(Event::Key(alt('c'))), None);

        assert!(!h.app.phase.is_draining());
        assert_eq!(*h.builds.borrow(), ["started", "cancelled"]);
        assert_eq!(h.app.panes[0].state(), SessionState::AwaitingApproval);
        h.approve();
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
        assert!(h.session().iter().all(|entry| *entry != Entry::TurnParked));
    }

    #[tokio::test]
    async fn a_turn_that_parks_just_after_a_drain_is_abandoned_carries_on() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
        h.approve();
        h.app.handle(Event::Key(alt('c')));
        h.app.panes[0].set_paused(true);
        h.until(|event| matches!(event, Event::TurnEnded(_))).await;
        h.app.panes[0].set_paused(false);
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
    }

    #[tokio::test]
    async fn nothing_new_starts_during_a_drain_but_the_person_can_still_move_around() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;

        h.enter("hello");
        assert!(h.screen().contains("the harness is stopping"));
        assert_eq!(h.app.panes[1].state(), SessionState::Idle);
        h.enter("/new");
        h.enter("/rebuild");
        h.enter("/quit");
        h.press('x');
        assert_eq!(h.app.panes.len(), 2);
        assert_eq!(*h.builds.borrow(), ["started"]);

        h.press('g');
        assert_eq!(h.shown(), "first");
        assert!(h.screen().contains("[y/N]"));
    }

    #[tokio::test]
    async fn the_narrow_status_line_says_what_a_drain_is_doing() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
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
        let entries = SessionLog::open(&path_of(&h.dir, "first")).unwrap().1;
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
        assert!(!h.app.phase.is_draining());
    }

    #[tokio::test]
    async fn a_failed_relaunch_says_so_and_turns_carry_on_here() {
        let mut h = two_steps();
        h.drain_with_a_question_open("/rebuild").await;
        h.app.handle(built());
        h.approve();
        assert_eq!(h.until_exit().await, Exit::Rebuild(binary()));

        h.app.relaunch_failed("exec failed");

        assert!(h.screen().contains("rebuild failed: exec failed"));
        assert_eq!(h.app.panes[0].state(), SessionState::Working);
        h.until_turn_ends().await;
        assert_eq!(h.app.panes[0].state(), SessionState::Idle);
    }
}
