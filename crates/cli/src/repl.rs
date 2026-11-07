//! The conversation with the person at the prompt, from typed line to Turn.

use std::fmt::Display;
use std::io::Write;

use sstack_core::agent::{Agent, TurnEnd};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::terminal::{InputLines, render_event, turn_end_text};

/// What a typed line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    /// Text to send to the model.
    Prompt(&'a str),
    /// The `/rebuild` Human Command.
    Rebuild,
    /// The `/quit` Human Command.
    Quit,
    /// A slash-prefixed line that is not a known Human Command.
    Unknown(&'a str),
    /// A blank line.
    Empty,
}

/// Works out what a typed line asks for.
///
/// Slash-prefixed lines never reach the model, even if the command is mistyped.
#[must_use]
pub fn parse_command(line: &str) -> Command<'_> {
    match line.trim() {
        "" => Command::Empty,
        "/rebuild" => Command::Rebuild,
        "/quit" | "/exit" => Command::Quit,
        other if other.starts_with('/') => Command::Unknown(other),
        other => Command::Prompt(other),
    }
}

/// Why the prompt loop stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// The person is finished.
    Quit,
    /// The person asked to rebuild the harness.
    Rebuild,
}

/// Carries on a conversation with the person at the terminal.
pub struct Repl<W> {
    /// Lines typed at the terminal.
    pub input: InputLines,
    /// One message for each Ctrl-C.
    pub interrupts: mpsc::Receiver<()>,
    /// Where output is written.
    pub out: W,
}

impl<W: Write + Send> Repl<W> {
    /// Runs until the person quits, input ends, or a rebuild is requested.
    pub async fn run(&mut self, agent: &mut Agent) -> Exit {
        loop {
            self.show("\n> ");
            let Some(line) = self.next_line().await else {
                return Exit::Quit;
            };
            if let Some(exit) = self.handle(agent, &line).await {
                return exit;
            }
        }
    }

    /// Acts on one typed line, returning a reason to exit if it calls for one.
    async fn handle(&mut self, agent: &mut Agent, line: &str) -> Option<Exit> {
        match parse_command(line) {
            Command::Prompt(text) => self.turn(agent, text).await,
            Command::Rebuild => return Some(Exit::Rebuild),
            Command::Quit => return Some(Exit::Quit),
            Command::Unknown(name) => self.show(format!("unknown command: {name}\n")),
            Command::Empty => {}
        }
        None
    }

    /// Reports a rebuild that could not go ahead, then resumes the prompt loop.
    pub fn report_rebuild_failure(&mut self, reason: impl Display) {
        self.show(format!("rebuild failed: {reason}\n"));
    }

    /// Waits for a line. An interrupt at the prompt counts as the end of input.
    async fn next_line(&mut self) -> Option<String> {
        let input = self.input.clone();
        let mut input = input.lock().await;
        tokio::select! {
            line = input.recv() => line,
            _ = self.interrupts.recv() => None,
        }
    }

    /// Runs one Turn, cancelling it if an interrupt arrives, and shows how it ended.
    async fn turn(&mut self, agent: &mut Agent, text: &str) {
        let cancel = CancellationToken::new();
        let outcome = {
            let Self {
                interrupts, out, ..
            } = self;
            let mut sink = |event| render_event(&event, out);
            let turn = agent.run_turn(text, &cancel, &mut sink);
            tokio::pin!(turn);
            let result = tokio::select! {
                result = &mut turn => result,
                _ = interrupts.recv() => {
                    cancel.cancel();
                    turn.await
                }
            };
            outcome_text(result)
        };
        self.show(outcome);
    }

    /// Shows `text` to the person. Failures are ignored, because they must not stop a Turn.
    fn show(&mut self, text: impl Display) {
        let _ = write!(self.out, "{text}");
        let _ = self.out.flush();
    }
}

/// Describes how a Turn ended, for the person.
///
/// Provider failures end the Turn rather than the Session, so the person can simply try again.
pub fn outcome_text(result: Result<TurnEnd, impl Display>) -> String {
    match result {
        Ok(end) => turn_end_text(end).map_or_else(String::new, |text| format!("\n{text}\n")),
        Err(error) => format!("\nerror: {error}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sstack_core::agent::AgentConfig;
    use sstack_core::provider::scripted::{Scripted, ScriptedProvider};
    use sstack_core::provider::{ModelRequest, Provider, ProviderError, StreamEvent};
    use sstack_core::session::{AssistantEntry, Entry, SessionLog, StopReason, Usage};
    use sstack_core::tools::project::Project;
    use sstack_core::tools::{DenyAll, ToolBox};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn reply(text: &str) -> AssistantEntry {
        AssistantEntry {
            text: text.into(),
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
            payload: None,
        }
    }

    fn agent(dir: &tempfile::TempDir, script: Vec<Scripted>) -> Agent {
        agent_with(dir, Arc::new(ScriptedProvider::new(script)))
    }

    fn agent_with(dir: &tempfile::TempDir, provider: Arc<dyn Provider>) -> Agent {
        let project = Arc::new(Project::new(dir.path()).unwrap());
        let config = AgentConfig {
            provider,
            model: "scripted".into(),
            system: String::new(),
            tools: ToolBox::standard(project, Arc::new(DenyAll), vec![]),
            context_limit: 1000,
        };
        let log = SessionLog::create(&dir.path().join("session.jsonl")).unwrap();
        Agent::new(config, log, vec![]).unwrap()
    }

    struct Terminal {
        lines: mpsc::Sender<String>,
        interrupts: mpsc::Sender<()>,
        repl: Repl<Vec<u8>>,
    }

    fn terminal() -> Terminal {
        let (lines, input) = mpsc::channel(8);
        let (interrupts, interrupted) = mpsc::channel(8);
        Terminal {
            lines,
            interrupts,
            repl: Repl {
                input: Arc::new(Mutex::new(input)),
                interrupts: interrupted,
                out: Vec::new(),
            },
        }
    }

    impl Terminal {
        async fn typed(&self, lines: &[&str]) {
            for line in lines {
                self.lines.send((*line).to_owned()).await.unwrap();
            }
        }

        fn output(&self) -> String {
            String::from_utf8(self.repl.out.clone()).unwrap()
        }
    }

    #[test]
    fn lines_parse_into_commands() {
        assert_eq!(parse_command(" hello "), Command::Prompt("hello"));
        assert_eq!(parse_command("/rebuild"), Command::Rebuild);
        assert_eq!(parse_command("/quit"), Command::Quit);
        assert_eq!(parse_command("/exit"), Command::Quit);
        assert_eq!(parse_command("/nope"), Command::Unknown("/nope"));
        assert_eq!(parse_command("   "), Command::Empty);
    }

    #[tokio::test]
    async fn prompts_run_turns_until_quit() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = agent(&dir, vec![Ok(reply("hello back"))]);
        let mut terminal = terminal();
        terminal.typed(&["", "/nope", "hello", "/quit"]).await;

        let exit = terminal.repl.run(&mut agent).await;

        assert_eq!(exit, Exit::Quit);
        let output = terminal.output();
        assert!(output.contains("unknown command: /nope"));
        assert!(output.contains("hello back"));
        assert_eq!(agent.entries().len(), 2);
    }

    #[tokio::test]
    async fn rebuild_stops_the_loop_for_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = agent(&dir, vec![]);
        let mut terminal = terminal();
        terminal.typed(&["/rebuild"]).await;
        assert_eq!(terminal.repl.run(&mut agent).await, Exit::Rebuild);
        terminal.repl.report_rebuild_failure("no compiler");
        assert!(terminal.output().contains("rebuild failed: no compiler"));
    }

    #[tokio::test]
    async fn closed_input_and_interrupts_at_the_prompt_quit() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = agent(&dir, vec![]);

        let mut closed = terminal();
        drop(closed.lines);
        assert_eq!(closed.repl.run(&mut agent).await, Exit::Quit);

        let mut interrupted = terminal();
        interrupted.interrupts.send(()).await.unwrap();
        assert_eq!(interrupted.repl.run(&mut agent).await, Exit::Quit);
    }

    /// Interrupts as soon as a request starts, then never answers it.
    struct InterruptedRequest(mpsc::Sender<()>);

    #[async_trait]
    impl Provider for InterruptedRequest {
        async fn respond(
            &self,
            _request: ModelRequest<'_>,
            _on_event: &mut (dyn FnMut(StreamEvent) + Send),
        ) -> Result<AssistantEntry, ProviderError> {
            self.0.send(()).await.unwrap();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn an_interrupt_during_a_turn_cancels_it_and_the_session_continues() {
        let dir = tempfile::tempdir().unwrap();
        let mut terminal = terminal();
        let provider = Arc::new(InterruptedRequest(terminal.interrupts.clone()));
        let mut agent = agent_with(&dir, provider);
        terminal.typed(&["go", "/quit"]).await;

        let exit = terminal.repl.run(&mut agent).await;

        assert_eq!(exit, Exit::Quit);
        assert!(terminal.output().contains("[cancelled]"));
        assert_eq!(agent.entries().last(), Some(&Entry::TurnCancelled));
    }

    #[test]
    fn outcomes_describe_failures_and_unusual_ends() {
        assert_eq!(outcome_text(Ok::<_, String>(TurnEnd::Finished)), "");
        assert!(outcome_text(Ok::<_, String>(TurnEnd::Cancelled)).contains("cancelled"));
        let failed = outcome_text(Err::<TurnEnd, _>("down"));
        assert_eq!(failed, "\nerror: down\n");
    }
}
