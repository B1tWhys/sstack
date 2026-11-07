//! The interactive harness as a whole: it routes events to components and carries out what they
//! ask for.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout};
use ratatui::{Frame, Terminal};

use super::component::{Component, KeyOutcome};
use super::components::{Approval, Prompt, Status, Transcript};
use super::event::{Action, Event, EventReceiver};
use super::worker::AgentHandle;
use crate::command::{Command, parse_command};

/// Tells the person why their input was not acted on.
const BUSY: &str = "a Turn is running: wait for it to end, or press Ctrl-C to cancel it";

/// Why the interactive harness stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// The person is finished.
    Quit,
    /// The person asked for a Rebuild. The App can be run again if the Rebuild fails.
    Rebuild,
}

/// Runs the interactive harness: the one place that knows every component, the agent, and
/// what each [`Action`] means.
pub struct App {
    /// Asks about shell commands, over everything else.
    approval: Approval,
    /// Where the person types.
    prompt: Prompt,
    /// The Session so far.
    transcript: Transcript,
    /// The Session id and what the agent is doing.
    status: Status,
    /// Starts and cancels Turns.
    agent: AgentHandle,
    /// Whether a Turn has been started and has not yet ended.
    busy: bool,
}

impl App {
    /// Creates the harness for the Session with id `session`, run by `agent`.
    pub fn new(agent: AgentHandle, session: String) -> Self {
        Self {
            approval: Approval::default(),
            prompt: Prompt::default(),
            transcript: Transcript::default(),
            status: Status::new(session),
            agent,
            busy: false,
        }
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
            Event::Key(key) => self.key(key),
            event => {
                self.broadcast(&event);
                None
            }
        }
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
    fn broadcast(&mut self, event: &Event) {
        if matches!(event, Event::TurnEnded(_)) {
            self.busy = false;
        }
        for component in self.components() {
            component.update(event);
        }
    }

    /// Offers a key press to each component in turn, after the keys that work everywhere.
    fn key(&mut self, key: KeyEvent) -> Option<Exit> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return self.act(Action::Interrupt);
        }
        let outcome = self
            .components()
            .into_iter()
            .map(|component| component.key(key))
            .find(|outcome| *outcome != KeyOutcome::Ignored);
        match outcome {
            Some(KeyOutcome::Act(action)) => self.act(action),
            _ => None,
        }
    }

    /// Carries out what a component asked for.
    fn act(&mut self, action: Action) -> Option<Exit> {
        match action {
            Action::Submit(line) => self.submit(&line),
            Action::Interrupt if self.busy => {
                self.agent.cancel();
                None
            }
            Action::Interrupt => Some(Exit::Quit),
        }
    }

    /// Acts on a line the person entered.
    fn submit(&mut self, line: &str) -> Option<Exit> {
        let command = parse_command(line);
        if self.busy && matches!(command, Command::Prompt(_) | Command::Rebuild) {
            self.notice(BUSY.to_owned());
            return None;
        }
        match command {
            Command::Prompt(text) => self.start_turn(text),
            Command::Rebuild => return Some(Exit::Rebuild),
            Command::Quit => return Some(Exit::Quit),
            Command::Unknown(name) => self.notice(format!("unknown command: {name}")),
            Command::Empty => {}
        }
        None
    }

    /// Asks the agent to run a Turn on `text`.
    fn start_turn(&mut self, text: &str) {
        self.busy = true;
        self.agent.prompt(text.to_owned());
    }

    /// Tells the person something from the harness itself.
    pub fn notice(&mut self, text: String) {
        self.broadcast(&Event::Notice(text));
    }

    /// Draws every component in its place, with any question for the person on top.
    ///
    /// The prompt grows with what is typed, up to half the screen.
    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
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
        self.approval.render(frame, frame.area());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::approver::EventApprover;
    use crate::tui::event::EventSender;
    use crate::tui::test_support::{agent, bash_call, ctrl, key, reply, rows};
    use async_trait::async_trait;
    use ratatui::backend::TestBackend;
    use sstack_core::provider::scripted::{Scripted, ScriptedProvider};
    use sstack_core::provider::{ModelRequest, Provider, ProviderError, StreamEvent};
    use sstack_core::session::{AssistantEntry, Entry, SessionLog};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// An App wired to a real agent, with the events it would receive held back for the test.
    struct Harness {
        dir: tempfile::TempDir,
        app: App,
        sender: EventSender,
        events: EventReceiver,
    }

    fn harness(provider: Arc<dyn Provider>) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let (sender, events) = mpsc::unbounded_channel();
        let approver = Arc::new(EventApprover::new(sender.clone()));
        let agent = AgentHandle::spawn(agent(dir.path(), provider, approver), sender.clone());
        Harness {
            dir,
            app: App::new(agent, "7".into()),
            sender,
            events,
        }
    }

    fn scripted(script: Vec<Scripted>) -> Harness {
        harness(Arc::new(ScriptedProvider::new(script)))
    }

    impl Harness {
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
                let done = last(&event);
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

        fn screen(&self) -> String {
            let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
            terminal.draw(|frame| self.app.render(frame)).unwrap();
            rows(&terminal).join("\n")
        }

        fn session(&self) -> Vec<Entry> {
            SessionLog::open(&self.dir.path().join("session.jsonl"))
                .unwrap()
                .1
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
        assert!(screen.contains("session 7 · ready"), "{screen}");
        assert_eq!(h.session().len(), 2);
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
        assert!(rows[5].starts_with("session 7"), "{screen}");
        assert!(rows[6].starts_with("┌"), "{screen}");
        assert!(rows[11].starts_with("└"), "{screen}");

        h.app.handle(Event::Paste("\n".repeat(20)));
        assert!(h.screen().lines().nth(5).unwrap().starts_with("session 7"));
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
        assert!(rows(&terminal).join("\n").contains("session 7"));
    }

    #[tokio::test]
    async fn the_loop_quits_once_nothing_can_send_events() {
        let (_, mut events) = mpsc::unbounded_channel();
        let mut h = scripted(vec![]);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let exit = h.app.run(&mut terminal, &mut events).await;
        assert_eq!(exit, Ok(Exit::Quit));
    }
}
