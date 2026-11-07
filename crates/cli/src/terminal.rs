//! Talking to the person at the terminal.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
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

/// Starts the look given to the model's reasoning: dim and italic.
const REASONING_STYLE: &str = "\x1b[2;3m";
/// Returns the terminal to its normal look.
const RESET_STYLE: &str = "\x1b[0m";

/// Returns whether output to `stream` may be styled.
///
/// Only a terminal is styled, and never when the `NO_COLOR` variable is set to anything non-empty.
pub fn wants_style(stream: &impl IsTerminal) -> bool {
    style_allowed(stream.is_terminal(), std::env::var_os("NO_COLOR"))
}

/// Decides on styling from whether the output is a terminal and the value of `NO_COLOR`, if set.
fn style_allowed(is_terminal: bool, no_color: Option<OsString>) -> bool {
    is_terminal && no_color.is_none_or(|value| value.is_empty())
}

/// Shows a Turn's events to the person, setting the model's reasoning apart from its reply.
///
/// Use one for each Turn, because it remembers what was shown last. Write failures are ignored,
/// because they must not stop a Turn.
pub struct EventView<W> {
    /// Where events are shown.
    out: W,
    /// Whether reasoning may be shown with terminal styling. Without it, reasoning is plain text.
    styled: bool,
    /// Whether the last thing shown was reasoning, so that what follows starts on a new line.
    in_reasoning: bool,
}

impl<W: Write> EventView<W> {
    /// Creates a view that writes to `out`, styling reasoning only if `styled` is set.
    pub fn new(out: W, styled: bool) -> Self {
        Self {
            out,
            styled,
            in_reasoning: false,
        }
    }

    /// Shows one event.
    pub fn show(&mut self, event: &AgentEvent) {
        let text = self.text_for(event);
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }

    /// Returns the text shown for `event`, given what was shown before it.
    fn text_for(&mut self, event: &AgentEvent) -> String {
        let is_reasoning = matches!(event, AgentEvent::Reasoning(_));
        let separator = if self.in_reasoning && !is_reasoning {
            "\n"
        } else {
            ""
        };
        self.in_reasoning = is_reasoning;
        format!("{separator}{}", event_text(event, self.styled))
    }
}

/// Returns the text shown for an agent event on its own, styling reasoning if `styled` is set.
fn event_text(event: &AgentEvent, styled: bool) -> String {
    match event {
        AgentEvent::Text(text) => text.clone(),
        AgentEvent::Reasoning(text) => reasoning_text(text, styled),
        AgentEvent::ToolStarted(call) => {
            format!("\n[{}] {}\n", call.name, shorten(&call.input))
        }
        AgentEvent::ToolFinished { name, is_error } => tool_finished(name, *is_error),
        AgentEvent::ContextNearLimit { used, limit } => {
            format!("\n[warning] context is {used} of {limit} tokens; start a new Session soon\n")
        }
    }
}

/// Returns a piece of reasoning as shown, styled if `styled` is set.
///
/// Each styled piece ends by resetting the style, so a Turn cut short never leaves the terminal
/// styled.
fn reasoning_text(text: &str, styled: bool) -> String {
    if styled {
        format!("{REASONING_STYLE}{text}{RESET_STYLE}")
    } else {
        text.to_owned()
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
        let events = [
            AgentEvent::Text("hi".into()),
            started,
            failed,
            passed,
            warning,
        ];

        let text = shown(&events, true);

        assert!(text.starts_with("hi\n[read] xxx"));
        assert!(text.contains("...\n[bash] failed\n"));
        assert!(text.contains("8 of 10 tokens"));
    }

    fn shown(events: &[AgentEvent], styled: bool) -> String {
        let mut out = Vec::new();
        let mut view = EventView::new(&mut out, styled);
        for event in events {
            view.show(event);
        }
        String::from_utf8(out).unwrap()
    }

    fn thinking_then_replying() -> [AgentEvent; 3] {
        [
            AgentEvent::Reasoning("let me ".into()),
            AgentEvent::Reasoning("think".into()),
            AgentEvent::Text("Done.".into()),
        ]
    }

    #[test]
    fn styled_reasoning_is_dim_and_italic_and_never_leaks_its_style() {
        let text = shown(&thinking_then_replying(), true);
        assert_eq!(text, "\x1b[2;3mlet me \x1b[0m\x1b[2;3mthink\x1b[0m\nDone.",);
    }

    #[test]
    fn plain_reasoning_has_no_escape_codes_but_still_ends_its_line() {
        assert_eq!(
            shown(&thinking_then_replying(), false),
            "let me think\nDone."
        );
    }

    #[test]
    fn a_tool_call_after_reasoning_starts_on_a_new_line() {
        let events = [
            AgentEvent::Reasoning("check".into()),
            AgentEvent::ToolStarted(ToolCallSummary {
                name: "bash".into(),
                input: "ls".into(),
            }),
        ];
        assert_eq!(shown(&events, false), "check\n\n[bash] ls\n");
    }

    #[test]
    fn only_a_terminal_without_no_color_is_styled() {
        assert!(style_allowed(true, None));
        assert!(style_allowed(true, Some(OsString::new())));
        assert!(!style_allowed(true, Some("1".into())));
        assert!(!style_allowed(false, None));
        assert!(!wants_style(&tempfile::tempfile().unwrap()));
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
