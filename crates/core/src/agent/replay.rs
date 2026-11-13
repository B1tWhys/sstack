//! What a resumed Session shows of its log.

use std::collections::HashSet;

use crate::session::{AssistantEntry, Entry};
use crate::session_report::Replayed;

use super::turn::{TurnEnd, calls_not_started};

/// Returns what `entries` show, in order, for an interface to draw a resumed Session as it drew
/// it live.
///
/// The log does not record the model's thinking or errors that stopped a Turn, so those are
/// missing. A Turn that a Rebuild put off shows as it was before, so its calls that never started
/// are left out.
#[must_use]
pub fn replay(entries: &[Entry]) -> Vec<Replayed> {
    let not_started = calls_not_started(entries);
    let mut shown = Vec::new();
    for entry in entries {
        match entry {
            Entry::User { text } => shown.push(Replayed::Prompt(text.clone())),
            Entry::Assistant(response) => replay_response(response, &not_started, &mut shown),
            Entry::ToolResult {
                call_id, is_error, ..
            } => shown.push(Replayed::ToolFinished {
                id: call_id.clone(),
                is_error: *is_error,
            }),
            Entry::TurnCancelled => shown.push(Replayed::TurnEnded(TurnEnd::Cancelled)),
            Entry::TurnParked => shown.push(Replayed::TurnEnded(TurnEnd::Paused)),
            Entry::TurnPutOff { .. } => shown.push(Replayed::TurnEnded(TurnEnd::PutOff)),
        }
    }
    shown
}

/// Adds to `shown` what a recorded model response shows: its reply, the tools it called apart
/// from those in `not_started`, and how it ended.
fn replay_response(
    response: &AssistantEntry,
    not_started: &HashSet<String>,
    shown: &mut Vec<Replayed>,
) {
    if !response.text.is_empty() {
        shown.push(Replayed::Reply(response.text.clone()));
    }
    let started = (response.tool_calls.iter()).filter(|call| !not_started.contains(&call.id));
    shown.extend(started.map(|call| Replayed::ToolStarted {
        id: call.id.clone(),
        call: call.into(),
    }));
    shown.push(Replayed::TurnEnded(response.stop.into()));
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use serde_json::json;

    use super::*;
    use crate::agent::ToolCallSummary;
    use crate::session::{StopReason, ToolCall};

    /// Returns a response that says `text` and calls `read` once for each id in `calls`.
    fn response(text: &str, calls: &[&str], stop: StopReason) -> Entry {
        let calls = calls.iter().map(|id| ToolCall {
            id: (*id).to_owned(),
            name: "read".to_owned(),
            input: json!({}),
        });
        Entry::Assistant(AssistantEntry {
            text: text.to_owned(),
            tool_calls: calls.collect(),
            stop,
            usage: crate::session::Usage::default(),
            payload: None,
        })
    }

    /// Returns what a started `read` call with id `id` shows.
    fn started(id: &str) -> Replayed {
        Replayed::ToolStarted {
            id: id.to_owned(),
            call: ToolCallSummary {
                name: "read".to_owned(),
                input: "{}".to_owned(),
            },
        }
    }

    #[test]
    fn a_log_shows_what_was_said_called_and_how_turns_ended() {
        let entries = [
            Entry::User {
                text: "hi".to_owned(),
            },
            response("", &["1"], StopReason::ToolUse),
            Entry::ToolResult {
                call_id: "1".to_owned(),
                output: "nope".to_owned(),
                is_error: true,
            },
            response("done", &[], StopReason::MaxTokens),
            Entry::TurnCancelled,
            Entry::TurnParked,
        ];
        assert_eq!(
            replay(&entries),
            [
                Replayed::Prompt("hi".to_owned()),
                started("1"),
                Replayed::TurnEnded(TurnEnd::Finished),
                Replayed::ToolFinished {
                    id: "1".to_owned(),
                    is_error: true,
                },
                Replayed::Reply("done".to_owned()),
                Replayed::TurnEnded(TurnEnd::Truncated),
                Replayed::TurnEnded(TurnEnd::Cancelled),
                Replayed::TurnEnded(TurnEnd::Paused),
            ]
        );
    }

    #[test]
    fn calls_after_a_put_off_question_are_left_out() {
        let entries = [
            response("", &["1", "2"], StopReason::ToolUse),
            Entry::TurnPutOff {
                waiting_since: SystemTime::UNIX_EPOCH,
                queued: None,
            },
        ];
        assert_eq!(
            replay(&entries),
            [
                started("1"),
                Replayed::TurnEnded(TurnEnd::Finished),
                Replayed::TurnEnded(TurnEnd::PutOff),
            ]
        );
    }
}
