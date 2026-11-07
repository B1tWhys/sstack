//! The interactive harness as a whole: it keeps every Session, routes events to them and carries
//! out what they ask for.

use std::path::Path;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::{Frame, Terminal};
use sstack_core::workspace::SessionSummary;

use super::component::{Component, KeyOutcome};
use super::components::{Activity, Row, SIDEBAR_WIDTH, Sidebar};
use super::event::{Action, Event, EventReceiver};
use super::lifecycle::{self, Interrupt};
use super::pane::{Opened, Pane};
use super::theme::ColorDepth;
use crate::AnyError;
use crate::command::{Command, parse_command};

/// Tells the person why their input was not acted on.
const BUSY: &str = "a Turn is running: wait for it to end, or press Ctrl-C to cancel it";
/// Tells the person why the harness did not rebuild.
const BUSY_ELSEWHERE: &str = "a Turn is running in another Session: wait for it to end first";
/// Tells the person why Ctrl-C did not quit.
const OTHERS_WORKING: &str = "other Sessions are working: use /quit to stop them too";
/// The narrowest screen that shows the sidebar, which would otherwise crowd out the Session.
const SIDEBAR_MIN_SCREEN_WIDTH: u16 = 80;

/// The keys that work whatever the Session on screen is doing, even with a question open, so the
/// person can always get away to something more urgent.
const GLOBAL_KEYS: [(char, Action); 4] = [
    ('c', Action::Interrupt),
    ('n', Action::NextSession),
    ('p', Action::PreviousSession),
    ('g', Action::LongestWaiting),
];

/// Opens a Session for the App: the one with the given id, or a new one if there is no id.
pub type Opener<'a> = Box<dyn FnMut(Option<&str>) -> Result<Opened, AnyError> + 'a>;

/// Why the interactive harness stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// The person is finished.
    Quit,
    /// The person asked for a Rebuild. The App can be run again if the Rebuild fails.
    Rebuild,
}

/// Runs the interactive harness: the one place that knows every Session and what each [`Action`]
/// means.
pub struct App<'a> {
    /// Every Session the person can switch to, in no meaningful order.
    panes: Vec<Pane>,
    /// The position in `panes` of the Session on screen.
    shown: usize,
    /// Lists the Sessions.
    sidebar: Sidebar,
    /// Opens Sessions as the person asks for them.
    opener: Opener<'a>,
    /// How many colors the terminal can show, which the theme's colors are fitted to.
    colors: ColorDepth,
}

impl<'a> App<'a> {
    /// Creates the harness showing `first`, with every Session in `stored` a key press away, using
    /// `opener` to start the ones the person turns to, and drawing in the colors of `colors`.
    pub fn new(
        (first, stored): (Opened, Vec<SessionSummary>),
        opener: Opener<'a>,
        colors: ColorDepth,
    ) -> Self {
        let mut panes: Vec<Pane> = stored.into_iter().map(Pane::stored).collect();
        let found = panes.iter().position(|pane| pane.path() == first.path);
        let shown = found.unwrap_or(panes.len());
        match panes.get_mut(shown) {
            Some(pane) => pane.attach(first.agent),
            None => panes.push(Pane::opened(first)),
        }
        Self {
            panes,
            shown,
            sidebar: Sidebar::default(),
            opener,
            colors,
        }
    }

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
            let Some(event) = events.recv().await else {
                return Ok(Exit::Quit);
            };
            if let Some(exit) = self.handle_waiting(event, events) {
                return Ok(exit);
            }
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
            Event::Session(id, event) => self.pass_on(&id, &event),
            Event::Click { column, row } => self.click(column, row),
            event => self.panes[self.shown].update(&event),
        }
        None
    }

    /// Passes on something the Session with id `id` did.
    fn pass_on(&mut self, id: &str, event: &Event) {
        if let Some(index) = self.position(id) {
            self.panes[index].observe(event, index == self.shown);
        }
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
        }
        None
    }

    /// Acts on Ctrl-C.
    fn interrupt(&mut self) -> Option<Exit> {
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
    fn ensure_agent(&mut self, index: usize) -> Result<(), AnyError> {
        if !self.panes[index].has_agent() {
            let opened = (self.opener)(Some(self.panes[index].id()))?;
            self.panes[index].attach(opened.agent);
        }
        Ok(())
    }

    /// Acts on a line the person entered.
    fn submit(&mut self, line: &str) -> Option<Exit> {
        match parse_command(line) {
            Command::Prompt(text) => self.prompt(text),
            Command::Rebuild => return self.rebuild(),
            Command::New => self.open_new(),
            Command::Quit => return Some(Exit::Quit),
            Command::Unknown(name) => self.notice(format!("unknown command: {name}")),
            Command::Empty => {}
        }
        None
    }

    /// Starts a Turn on `text` in the Session on screen, if it is free to run one.
    fn prompt(&mut self, text: &str) {
        if self.panes[self.shown].state().in_turn() {
            self.notice(BUSY.to_owned());
        } else {
            self.panes[self.shown].start_turn(text);
        }
    }

    /// Asks to Rebuild, unless that would cut a Turn short.
    fn rebuild(&mut self) -> Option<Exit> {
        if self.panes[self.shown].state().in_turn() {
            self.notice(BUSY.to_owned());
            None
        } else if self.others_in_turn() {
            self.notice(BUSY_ELSEWHERE.to_owned());
            None
        } else {
            Some(Exit::Rebuild)
        }
    }

    /// Starts a new Session and puts it on screen, leaving the others running.
    fn open_new(&mut self) {
        match (self.opener)(None) {
            Ok(opened) => {
                self.panes.push(Pane::opened(opened));
                self.shown = self.panes.len() - 1;
            }
            Err(error) => self.notice(format!("could not start a new Session: {error}")),
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
        let others = (area.is_empty())
            .then(|| self.summary_of_others(&rows))
            .flatten();
        self.panes[self.shown].set_others(others);
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
        .find(|(bound, _)| *bound == character && key.modifiers == KeyModifiers::CONTROL)
        .map(|(_, action)| action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::event::EventSender;
    use crate::tui::lifecycle::SessionState;
    use crate::tui::test_support::{bash_call, ctrl, key, opened, reply, rows};
    use async_trait::async_trait;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use sstack_core::provider::scripted::{Scripted, ScriptedProvider};
    use sstack_core::provider::{ModelRequest, Provider, ProviderError, StreamEvent};
    use sstack_core::session::{AssistantEntry, Entry, SessionLog};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    /// A width that leaves room for the sidebar.
    const WIDE: u16 = 100;
    /// A width too narrow for the sidebar.
    const NARROW: u16 = 60;

    /// An App wired to real agents, with the events it would receive held back for the test.
    struct Harness {
        dir: TempDir,
        app: App<'static>,
        sender: EventSender,
        events: EventReceiver,
    }

    /// Returns where the Session with id `id` is stored in `dir`.
    fn path_of(dir: &TempDir, id: &str) -> PathBuf {
        dir.path().join(id).with_extension("jsonl")
    }

    /// Returns an opener whose Sessions all run on `provider`, numbering new ones from one.
    fn opener(dir: &TempDir, provider: Arc<dyn Provider>, sender: EventSender) -> Opener<'static> {
        let root = dir.path().to_owned();
        let mut created = 0;
        Box::new(move |resume| {
            let id = resume.map_or_else(
                || {
                    created += 1;
                    format!("new{created}")
                },
                str::to_owned,
            );
            let path = root.join(id).with_extension("jsonl");
            Ok(opened(&path, provider.clone(), &sender))
        })
    }

    fn harness(provider: Arc<dyn Provider>) -> Harness {
        start(tempfile::tempdir().unwrap(), provider, vec![])
    }

    /// Starts an App in `dir` showing a new Session, with `stored` a key press away.
    fn start(dir: TempDir, provider: Arc<dyn Provider>, stored: Vec<SessionSummary>) -> Harness {
        let (sender, events) = mpsc::unbounded_channel();
        let first = opened(&path_of(&dir, "first"), provider.clone(), &sender);
        let open = opener(&dir, provider, sender.clone());
        let app = App::new((first, stored), open, ColorDepth::Ansi256);
        Harness {
            dir,
            app,
            sender,
            events,
        }
    }

    fn scripted(script: Vec<Scripted>) -> Harness {
        harness(Arc::new(ScriptedProvider::new(script)))
    }

    impl Harness {
        /// Replaces the App with a new one on the same Session, as a Rebuild does.
        fn rebuilt(self) -> Harness {
            start(self.dir, Arc::new(ScriptedProvider::new(vec![])), vec![])
        }

        /// Presses a key that goes to the App rather than to the prompt.
        fn press(&mut self, character: char) -> Option<Exit> {
            self.app.handle(Event::Key(ctrl(character)))
        }

        /// Starts a new Session, which leaves the keyboard's prompt ready for it.
        fn enter_new(&mut self) {
            assert_eq!(self.enter("/new"), None);
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

        fn screen_of(&mut self, width: u16) -> String {
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            terminal.draw(|frame| self.app.render(frame)).unwrap();
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

        let screen = h.screen();
        assert!(screen.contains("> hello"), "{screen}");
        assert!(screen.contains("hello back"), "{screen}");
        assert!(screen.contains("session first · ready"), "{screen}");
        assert_eq!(h.session().len(), 2);
    }

    #[tokio::test]
    async fn after_a_rebuild_the_session_so_far_is_shown() {
        let mut h = scripted(vec![Ok(reply("hello back"))]);
        h.enter("hello");
        h.until_turn_ends().await;

        let mut h = h.rebuilt();
        h.until(|event| matches!(event, Event::Resumed(_))).await;

        let screen = h.screen();
        assert!(screen.contains("> hello"), "{screen}");
        assert!(screen.contains("hello back"), "{screen}");
        assert!(screen.contains("session first · ready"), "{screen}");
    }

    #[tokio::test]
    async fn human_commands_never_reach_the_model() {
        let mut h = scripted(vec![]);
        assert_eq!(h.enter(""), None);
        assert_eq!(h.enter("/nope"), None);
        assert!(h.screen().contains("unknown command: /nope"));
        assert_eq!(h.enter("/rebuild"), Some(Exit::Rebuild));
        assert_eq!(h.enter("/quit"), Some(Exit::Quit));
        assert_eq!(h.session(), Vec::new());
    }

    #[tokio::test]
    async fn an_approved_command_runs_and_the_turn_carries_on() {
        let script = vec![Ok(bash_call("touch ran")), Ok(reply("all done"))];
        let mut h = scripted(script);
        h.enter("go");
        h.until(|event| matches!(event, Event::Approval(_))).await;
        assert!(h.screen().contains("Run `touch ran`? [y/N]"));

        assert_eq!(h.app.handle(Event::Key(key(KeyCode::Char('y')))), None);
        h.until_turn_ends().await;

        assert!(h.dir.path().join("ran").exists());
        let screen = h.screen();
        assert!(screen.contains("[bash]"), "{screen}");
        assert!(screen.contains("all done"), "{screen}");
        assert!(!screen.contains("[y/N]"), "{screen}");
    }

    /// Never answers, so that a Turn stays running until it is cancelled.
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
        assert_eq!(h.enter("/rebuild"), None);
        assert!(h.screen().contains("a Turn is running"));

        assert_eq!(h.app.handle(Event::Key(ctrl('c'))), None);
        h.until_turn_ends().await;
        assert!(h.screen().contains("[cancelled]"));
        assert_eq!(h.session().last(), Some(&Entry::TurnCancelled));

        assert_eq!(h.app.handle(Event::Key(ctrl('c'))), Some(Exit::Quit));
    }

    #[tokio::test]
    async fn the_prompt_grows_with_long_input_up_to_half_the_screen() {
        let mut h = scripted(vec![]);
        let typed = "word ".repeat(40);
        h.app.handle(Event::Paste(typed.clone()));
        let screen = h.screen();
        let rows: Vec<&str> = screen.lines().collect();
        assert_eq!(rows.len(), 12);
        assert!(rows[5].starts_with("session first"), "{screen}");
        assert!(rows[6].starts_with("┌"), "{screen}");
        assert!(rows[11].starts_with("└"), "{screen}");

        h.app.handle(Event::Paste("\n".repeat(20)));
        assert!(
            h.screen()
                .lines()
                .nth(5)
                .unwrap()
                .starts_with("session first")
        );
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
            first_prompt: text,
            modified: SystemTime::now() - Duration::from_secs(age),
        }
    }

    /// Starts an App with two stored Sessions as well as the new one.
    fn with_stored(provider: Arc<dyn Provider>) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let stored = vec![summary(&dir, "younger", 100), summary(&dir, "older", 200)];
        start(dir, provider, stored)
    }

    fn sidebar_order(h: &mut Harness) -> Vec<String> {
        let screen = h.screen_of(WIDE);
        let rows = screen.lines().skip(1);
        let titles = rows.map(|row| row.chars().skip(4).take(17).collect::<String>());
        titles
            .map(|title| title.trim().to_owned())
            .take_while(|title| !title.is_empty())
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
        let wide = h.screen_of(WIDE);
        assert!(wide.contains("sessions"), "{wide}");
        let narrow = h.screen_of(NARROW);
        assert!(!narrow.contains("sessions  "), "{narrow}");
    }

    #[tokio::test]
    async fn keys_move_through_the_sidebar_and_wrap() {
        let mut h = with_stored(Arc::new(NeverResponds));
        h.press('n');
        assert_eq!(h.shown(), "older");
        h.press('n');
        assert_eq!(h.shown(), "younger");
        h.press('p');
        assert_eq!(h.shown(), "older");
        h.press('p');
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
        h.press('n');
        h.until(|event| matches!(event, Event::Resumed(entries) if !entries.is_empty()))
            .await;
        assert!(h.screen().contains("> older"));
    }

    #[tokio::test]
    async fn a_session_that_cannot_open_leaves_the_person_where_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let stored = vec![summary(&dir, "older", 100)];
        let (sender, events) = mpsc::unbounded_channel();
        let provider = Arc::new(NeverResponds);
        let first = opened(&path_of(&dir, "first"), provider, &sender);
        let open: Opener = Box::new(|_| Err("no keychain".into()));
        let app = App::new((first, stored), open, ColorDepth::Ansi256);
        let mut h = Harness {
            dir,
            app,
            sender,
            events,
        };

        h.press('n');
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
        h.press('n');
        assert_eq!(h.shown(), "first");
        assert!(!h.screen().contains("half typed"));
        h.press('n');
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

        assert_eq!(h.enter("/rebuild"), None);
        assert!(h.screen().contains("in another Session"));
    }

    #[tokio::test]
    async fn turns_end_unseen_until_the_session_is_viewed() {
        let mut h = scripted(vec![Ok(reply("all done"))]);
        h.enter("one");
        h.enter_new();
        h.until_turn_ends().await;
        let screen = h.screen_of(WIDE);
        assert!(screen.contains("+ one"), "{screen}");

        h.press('n');
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

        let screen = h.screen_of(WIDE);
        assert!(screen.contains("! go"), "{screen}");
        assert!(screen.contains("blocked"), "{screen}");
        assert!(!screen.contains("[y/N]"), "{screen}");

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
        h.press('n');
        h.enter("go");
        h.until(|event| matches!(event, Event::Approval(_))).await;
        h.press('n');
        assert_eq!(h.shown(), "new1");
    }

    #[tokio::test]
    async fn a_narrow_status_line_says_what_the_sidebar_would() {
        let mut h = scripted(vec![Ok(bash_call("touch ran"))]);
        h.enter("go");
        h.enter_new();
        h.until(|event| matches!(event, Event::Approval(_))).await;
        let narrow = h.screen();
        assert!(narrow.contains("1 blocked"), "{narrow}");
        let wide = h.screen_of(WIDE);
        assert!(!wide.contains("1 blocked"), "{wide}");
    }

    #[tokio::test]
    async fn events_from_an_unknown_session_are_ignored() {
        let mut h = harness(Arc::new(NeverResponds));
        let gone = Event::Session("gone".into(), Box::new(Event::Resize));
        assert_eq!(h.app.handle(gone), None);
    }

    #[test]
    fn only_control_keys_are_global() {
        assert_eq!(global_action(ctrl('n')), Some(Action::NextSession));
        assert_eq!(global_action(key(KeyCode::Char('n'))), None);
        assert_eq!(global_action(ctrl('x')), None);
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
}
