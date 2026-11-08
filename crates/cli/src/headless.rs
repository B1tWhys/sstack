//! The headless mode: one prompt run with nobody watching, as plain text on standard output.

use std::ffi::OsString;
use std::io::{IsTerminal, Write, stdout};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use sstack::agent::{AgentEvent, TurnContext, TurnEnd};
use sstack::sandbox::Containers;
use sstack::tools::DenyAll;
use sstack::workspace;
use tokio_util::sync::CancellationToken;

use crate::Args;
use crate::describe;
use crate::setup::{Place, Run, Start, prepare};

/// Runs one prompt with no interaction, denying all shell commands. With nobody to watch, it
/// has no Sandbox either.
///
/// Succeeds only if the Turn finished normally, so scripts can tell when it did not.
pub async fn run(args: &Args, prompt: &str) -> Result<ExitCode> {
    let start = args
        .resume
        .as_deref()
        .map_or(Start::New(Place::MainCheckout), Start::Resume);
    let Run {
        mut agent, session, ..
    } = prepare()
        .args(args)
        .start(start)
        .approver(|_| Arc::new(DenyAll))
        .containers(Arc::new(Containers::new(Vec::new())))
        .call()?;
    let cancel = CancellationToken::new();
    let out = stdout();
    let mut view = EventView::new(&out, wants_style(&out));
    let never_paused = AtomicBool::new(false);
    let mut context = TurnContext {
        cancel: &cancel,
        pause: &never_paused,
        on_event: &mut |event| view.show(&event),
    };
    let result = agent.run_turn(prompt, &mut context).await;
    let outcome =
        describe::outcome_text(result.as_ref().map(|end| *end).map_err(ToString::to_string));
    print!(
        "{}",
        outcome.map_or_else(String::new, |text| format!("\n{text}\n"))
    );
    println!("\n[session {}]", workspace::session_id(&session));
    Ok(exit_code(result.ok()))
}

/// Returns a success exit code only if the Turn finished normally.
fn exit_code(end: Option<TurnEnd>) -> ExitCode {
    match end {
        Some(TurnEnd::Finished) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// The escape sequence that starts the look given to the model's reasoning: dim and italic.
const REASONING_STYLE: &str = "\x1b[2;3m";
/// The escape sequence that returns the terminal to its normal look.
const RESET_STYLE: &str = "\x1b[0m";

/// Returns whether output to `stream` may be styled.
///
/// Only a terminal is styled, and never when the `NO_COLOR` variable is set to anything non-empty.
fn wants_style(stream: &impl IsTerminal) -> bool {
    style_allowed(stream.is_terminal(), std::env::var_os("NO_COLOR"))
}

/// Decides on styling from whether the output is a terminal and the value of `NO_COLOR`, if set.
fn style_allowed(is_terminal: bool, no_color: Option<OsString>) -> bool {
    is_terminal && no_color.is_none_or(|value| value.is_empty())
}

/// The text view of a Turn's events, which sets the model's reasoning apart from its reply.
///
/// Use one for each Turn, because it remembers what was shown last. Write failures are ignored,
/// because they must not stop a Turn.
struct EventView<W> {
    /// Where events are shown.
    out: W,
    /// Whether reasoning may be shown with terminal styling. Without it, reasoning is plain text.
    styled: bool,
    /// Whether the last thing shown was reasoning, so that what follows starts on a new line.
    in_reasoning: bool,
}

impl<W: Write> EventView<W> {
    /// Creates a view that writes to `out`, styling reasoning only if `styled` is set.
    fn new(out: W, styled: bool) -> Self {
        Self {
            out,
            styled,
            in_reasoning: false,
        }
    }

    /// Shows one event.
    fn show(&mut self, event: &AgentEvent) {
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
        AgentEvent::ToolStarted(call) => format!("\n{}\n", describe::tool_started(call)),
        AgentEvent::ToolFinished { name, is_error } => tool_finished(name, *is_error),
        AgentEvent::ContextNearLimit { used, limit } => {
            format!("\n{}\n", describe::context_warning(*used, *limit))
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
        format!("{}\n", describe::tool_failed(name))
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sstack::agent::ToolCallSummary;

    #[test]
    fn events_render_as_readable_text() {
        let started = AgentEvent::ToolStarted(ToolCallSummary {
            name: "read".into(),
            input: "x".repeat(300),
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
    fn only_a_finished_turn_is_a_success() {
        let code = |end| format!("{:?}", exit_code(end));
        assert_eq!(
            code(Some(TurnEnd::Finished)),
            format!("{:?}", ExitCode::SUCCESS)
        );
        assert_eq!(
            code(Some(TurnEnd::Refused)),
            format!("{:?}", ExitCode::FAILURE)
        );
        assert_eq!(code(None), format!("{:?}", ExitCode::FAILURE));
    }
}
