//! Talking to the person at the terminal.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use sstack_core::agent::{AgentEvent, TurnEnd};
use sstack_core::tools::Approver;
use tokio::sync::{Mutex, mpsc};

/// The most characters of a tool call's input to show.
const MAX_TOOL_INPUT_CHARS: usize = 200;

/// Lines typed at the terminal, shared because approvals interrupt the prompt loop.
pub type InputLines = Arc<Mutex<mpsc::Receiver<String>>>;

/// Lets the person decide at the terminal whether each shell command runs.
pub struct TerminalApprover<W> {
    /// The lines typed at the terminal, shared with the prompt loop.
    input: InputLines,
    /// Where approval questions are written.
    prompt: std::sync::Mutex<W>,
}

impl<W> TerminalApprover<W> {
    /// Creates an approver that reads answers from `input` and asks through `prompt`.
    pub fn new(input: InputLines, prompt: W) -> Self {
        Self {
            input,
            prompt: std::sync::Mutex::new(prompt),
        }
    }
}

#[async_trait]
impl<W: Write + Send> Approver for TerminalApprover<W> {
    async fn approve(&self, command: &str) -> bool {
        let shown = write!(self.prompt.lock().unwrap(), "\nRun `{command}`? [y/N] ");
        let answer = self.input.lock().await.recv().await;
        shown.is_ok() && answer.is_some_and(|line| is_yes(&line))
    }
}

/// Returns whether a typed answer means yes.
fn is_yes(line: &str) -> bool {
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Shows one agent event to the person. Failures are ignored, because they must not stop a Turn.
pub fn render_event(event: &AgentEvent, out: &mut impl Write) {
    let _ = out.write_all(event_text(event).as_bytes());
    let _ = out.flush();
}

/// Returns the text shown for an agent event.
fn event_text(event: &AgentEvent) -> String {
    match event {
        AgentEvent::Text(text) | AgentEvent::Reasoning(text) => text.clone(),
        AgentEvent::ToolStarted(call) => {
            format!("\n[{}] {}\n", call.name, shorten(&call.input))
        }
        AgentEvent::ToolFinished { name, is_error } => tool_finished(name, *is_error),
        AgentEvent::ContextNearLimit { used, limit } => {
            format!("\n[warning] context is {used} of {limit} tokens; start a new Session soon\n")
        }
    }
}

/// Returns the text shown when a tool finishes, which is nothing if it succeeded.
fn tool_finished(name: &str, is_error: bool) -> String {
    if is_error {
        format!("[{name}] failed\n")
    } else {
        String::new()
    }
}

/// Cuts long text to [`MAX_TOOL_INPUT_CHARS`], marking that it was cut.
fn shorten(text: &str) -> String {
    let short: String = text.chars().take(MAX_TOOL_INPUT_CHARS).collect();
    if short.len() < text.len() {
        format!("{short}...")
    } else {
        short
    }
}

/// Describes how a Turn ended, if that needs saying.
pub fn turn_end_text(end: TurnEnd) -> Option<&'static str> {
    match end {
        TurnEnd::Finished => None,
        TurnEnd::Truncated => Some("[stopped: the response hit the output limit]"),
        TurnEnd::Refused => Some("[stopped: the model declined this request]"),
        TurnEnd::Cancelled => Some("[cancelled]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sstack_core::agent::ToolCallSummary;

    #[tokio::test]
    async fn only_an_explicit_yes_approves() {
        for (answer, expected) in [("y", true), (" YES ", true), ("n", false), ("", false)] {
            let (sender, receiver) = mpsc::channel(1);
            sender.send(answer.to_owned()).await.unwrap();
            let approver = TerminalApprover::new(Arc::new(Mutex::new(receiver)), Vec::<u8>::new());
            assert_eq!(approver.approve("ls").await, expected, "{answer}");
            let prompt = approver.prompt.lock().unwrap().clone();
            assert!(String::from_utf8(prompt).unwrap().contains("`ls`"));
        }
    }

    #[tokio::test]
    async fn closed_input_denies() {
        let (sender, receiver) = mpsc::channel(1);
        drop(sender);
        let approver = TerminalApprover::new(Arc::new(Mutex::new(receiver)), Vec::<u8>::new());
        assert!(!approver.approve("ls").await);
    }

    #[test]
    fn events_render_as_readable_text() {
        let started = AgentEvent::ToolStarted(ToolCallSummary {
            name: "read".into(),
            input: "x".repeat(MAX_TOOL_INPUT_CHARS + 1),
        });
        let failed = AgentEvent::ToolFinished {
            name: "bash".into(),
            is_error: true,
        };
        let passed = AgentEvent::ToolFinished {
            name: "bash".into(),
            is_error: false,
        };
        let warning = AgentEvent::ContextNearLimit { used: 8, limit: 10 };
        let mut out = Vec::new();

        for event in [
            AgentEvent::Text("hi".into()),
            started,
            failed,
            passed,
            warning,
        ] {
            render_event(&event, &mut out);
        }

        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("hi\n[read] xxx"));
        assert!(text.contains("...\n[bash] failed\n"));
        assert!(text.contains("8 of 10 tokens"));
    }

    #[test]
    fn short_tool_input_is_shown_whole() {
        assert_eq!(shorten("short"), "short");
    }

    #[test]
    fn unusual_turn_ends_are_described() {
        assert_eq!(turn_end_text(TurnEnd::Finished), None);
        for end in [TurnEnd::Truncated, TurnEnd::Refused, TurnEnd::Cancelled] {
            assert!(turn_end_text(end).is_some());
        }
    }
}
