//! The words the person sees for what happens in a Session, whichever mode shows them.

use std::fmt::Display;

use sstack_core::agent::{ToolCallSummary, TurnEnd};

/// The most characters of a tool call's input to show.
const MAX_TOOL_INPUT_CHARS: usize = 200;

/// Describes a tool call that is about to run, cutting long input short.
pub fn tool_started(call: &ToolCallSummary) -> String {
    format!("[{}] {}", call.name, shorten(&call.input))
}

/// Describes a tool call that failed. Successful calls go unmentioned.
pub fn tool_failed(name: &str) -> String {
    format!("[{name}] failed")
}

/// Warns that the context window is nearly full, so the person can start a new Session before
/// requests begin to fail. Both counts are in tokens.
pub fn context_warning(used: u64, limit: u64) -> String {
    format!("[warning] context is {used} of {limit} tokens; start a new Session soon")
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

/// Describes how a Turn ended or the error that stopped it, if that needs saying.
///
/// Provider failures end the Turn rather than the Session, so the person can simply try again.
pub fn outcome_text(result: Result<TurnEnd, impl Display>) -> Option<String> {
    match result {
        Ok(end) => turn_end_text(end).map(str::to_owned),
        Err(error) => Some(format!("error: {error}")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_tool_input_is_cut_and_short_input_is_shown_whole() {
        let long = ToolCallSummary {
            name: "read".into(),
            input: "x".repeat(MAX_TOOL_INPUT_CHARS + 1),
        };
        let shown = tool_started(&long);
        assert!(shown.starts_with("[read] xxx"));
        assert!(shown.ends_with("x..."));
        assert_eq!(shorten("short"), "short");
    }

    #[test]
    fn failures_and_warnings_name_what_they_concern() {
        assert_eq!(tool_failed("bash"), "[bash] failed");
        assert!(context_warning(8, 10).contains("8 of 10 tokens"));
    }

    #[test]
    fn unusual_turn_ends_are_described() {
        assert_eq!(turn_end_text(TurnEnd::Finished), None);
        for end in [TurnEnd::Truncated, TurnEnd::Refused, TurnEnd::Cancelled] {
            assert!(turn_end_text(end).is_some());
        }
    }

    #[test]
    fn outcomes_describe_failures_and_unusual_ends() {
        assert_eq!(outcome_text(Ok::<_, String>(TurnEnd::Finished)), None);
        let cancelled = outcome_text(Ok::<_, String>(TurnEnd::Cancelled));
        assert_eq!(cancelled.as_deref(), Some("[cancelled]"));
        let failed = outcome_text(Err::<TurnEnd, _>("down"));
        assert_eq!(failed.as_deref(), Some("error: down"));
    }
}
