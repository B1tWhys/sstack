//! What has been said in a Session, as the window shows it.
//!
//! The TUI's `Transcript` and the CLI's `describe.rs` turn the same reports into words, and parts
//! of this module look much like them, such as the joining of streamed text and the cutting of
//! long tool input. By `judging-duplicate-code` they are not duplicate: a change to how one
//! interface presents a Session is not a bug in the other, and the GUI is expected to present
//! tool calls and reasoning differently once it gets a visual style (ADR-0009). So the GUI keeps
//! its own copy and its own words.

use std::collections::HashMap;

use iced::Element;
use iced::widget::text::Wrapping;
use iced::widget::{column, text};
use sstack::agent::{AgentEvent, ToolCallSummary, TurnEnd};
use sstack::session::{AssistantEntry, Entry};
use sstack::session_report::SessionReport;

/// The most characters of a tool call's input that its one-line summary shows.
const MAX_TOOL_INPUT_CHARS: usize = 100;

/// Space between the parts of the transcript, in logical pixels.
const SPACING: f32 = 10.0;

/// The Session as it unfolds in the window: what the Session held when it opened, what the
/// person sent, what the model thought and replied, the tools it called, and what the window
/// tells the person.
#[derive(Debug, Default)]
pub struct Transcript {
    /// Everything shown, oldest first.
    blocks: Vec<Block>,
}

/// One part of the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// What the person sent.
    Prompt(String),
    /// The model's reply in one Step, as much of it as has arrived.
    Reply(String),
    /// The model's summarized thinking in one Step, as much of it as has arrived.
    Reasoning(String),
    /// A tool call the model made.
    Tool {
        /// Which tool, with what input.
        call: ToolCallSummary,
        /// Whether the call failed. A call still running has not failed.
        failed: bool,
    },
    /// Something the window tells the person, such as how a Turn ended or why it could not do
    /// what they asked.
    Notice(String),
}

impl Transcript {
    /// Shows what the person sent.
    pub fn prompt(&mut self, text: String) {
        self.blocks.push(Block::Prompt(text));
    }

    /// Tells the person `words`.
    pub fn notice(&mut self, words: String) {
        self.blocks.push(Block::Notice(words));
    }

    /// Shows what core reported, returning whether the transcript changed.
    ///
    /// The entries of [`SessionReport::Resumed`] go before everything already shown, since they
    /// happened before the window opened.
    pub fn report(&mut self, report: &SessionReport) -> bool {
        match report {
            SessionReport::Resumed(entries) => self.resume(entries),
            SessionReport::Agent(event) => self.agent(event),
            SessionReport::TurnEnded(outcome) => self.ended(outcome.as_ref().map(|end| *end)),
            SessionReport::Notice(words) => {
                self.notice(words.clone());
                true
            }
            _ => false,
        }
    }

    /// Returns everything shown, oldest first.
    #[cfg(test)]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Draws the transcript as plain text, one block under another.
    pub fn view<'a, Message: 'a>(&'a self) -> Element<'a, Message> {
        column(self.blocks.iter().map(Block::view))
            .spacing(SPACING)
            .into()
    }

    /// Shows something that happened during a Turn, returning whether the transcript changed.
    fn agent(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::Text(piece) => return self.reply(piece),
            AgentEvent::Reasoning(piece) => self.reasoning(piece),
            AgentEvent::ToolStarted(call) => self.tool(call.clone()),
            AgentEvent::ToolFinished { is_error, .. } => {
                return *is_error && self.last_tool_failed();
            }
            AgentEvent::ContextNearLimit { used, limit } => {
                self.notice(format!(
                    "the context window is {used} of {limit} tokens full, so start a new \
                     Session soon"
                ));
            }
        }
        true
    }

    /// Adds `piece` to the reply that is arriving, or starts one, returning whether the
    /// transcript changed. An empty piece, as a response with no text before its tool calls
    /// can bring, starts no reply.
    fn reply(&mut self, piece: &str) -> bool {
        if piece.is_empty() {
            return false;
        }
        if let Some(Block::Reply(reply)) = self.blocks.last_mut() {
            reply.push_str(piece);
        } else {
            self.blocks.push(Block::Reply(piece.to_owned()));
        }
        true
    }

    /// Adds `piece` to the reasoning that is arriving, or starts some.
    fn reasoning(&mut self, piece: &str) {
        if let Some(Block::Reasoning(reasoning)) = self.blocks.last_mut() {
            reasoning.push_str(piece);
        } else {
            self.blocks.push(Block::Reasoning(piece.to_owned()));
        }
    }

    /// Shows a tool call, which has not failed yet.
    fn tool(&mut self, call: ToolCallSummary) {
        self.blocks.push(Block::Tool {
            call,
            failed: false,
        });
    }

    /// Marks the newest tool call as failed, returning whether there was one. Tools run one at a
    /// time, so the call that finished is the newest.
    fn last_tool_failed(&mut self) -> bool {
        let newest = self.blocks.iter_mut().rev().find_map(|block| match block {
            Block::Tool { failed, .. } => Some(failed),
            _ => None,
        });
        newest.map(|failed| *failed = true).is_some()
    }

    /// Notes how a Turn ended or the error that stopped it, returning whether that needed
    /// saying.
    fn ended(&mut self, outcome: Result<TurnEnd, &String>) -> bool {
        let words = match outcome {
            Ok(end) => turn_end_text(end).map(str::to_owned),
            Err(error) => Some(format!("the Turn failed: {error}")),
        };
        words.map(|words| self.notice(words)).is_some()
    }

    /// Shows the Turns the Session already held, before everything already shown, returning
    /// whether it held any.
    ///
    /// The Session does not record the model's thinking or errors that stopped a Turn, so those
    /// are missing. Tool calls left unfinished by a cancelled Turn show as failed.
    fn resume(&mut self, entries: &[Entry]) -> bool {
        let mut earlier = Self::default();
        let mut calls = HashMap::new();
        for entry in entries {
            earlier.replay(entry, &mut calls);
        }
        self.blocks.splice(0..0, earlier.blocks);
        !entries.is_empty()
    }

    /// Shows one recorded entry. `calls` maps the id of each tool call shown so far to where its
    /// block is, so that its result can mark it failed.
    fn replay<'a>(&mut self, entry: &'a Entry, calls: &mut HashMap<&'a str, usize>) {
        match entry {
            Entry::User { text } => self.prompt(text.clone()),
            Entry::Assistant(response) => self.replay_response(response, calls),
            Entry::ToolResult {
                call_id, is_error, ..
            } => self.replay_result(calls.get(call_id.as_str()), *is_error),
            Entry::TurnCancelled => self.replay_end(TurnEnd::Cancelled),
            Entry::TurnParked => self.replay_end(TurnEnd::Paused),
        }
    }

    /// Shows a recorded model response: its reply, the tools it called, and how it ended the Turn
    /// if that needs saying.
    fn replay_response<'a>(
        &mut self,
        response: &'a AssistantEntry,
        calls: &mut HashMap<&'a str, usize>,
    ) {
        if !response.text.is_empty() {
            self.reply(&response.text);
        }
        for call in &response.tool_calls {
            calls.insert(call.id.as_str(), self.blocks.len());
            self.tool(call.into());
        }
        self.replay_end(response.stop.into());
    }

    /// Marks the tool call whose block is at `index` as failed, if it is shown and `is_error`.
    fn replay_result(&mut self, index: Option<&usize>, is_error: bool) {
        let block = index.and_then(|index| self.blocks.get_mut(*index));
        if let (Some(Block::Tool { failed, .. }), true) = (block, is_error) {
            *failed = true;
        }
    }

    /// Notes how a recorded Turn ended, if that needs saying.
    fn replay_end(&mut self, end: TurnEnd) {
        self.ended(Ok(end));
    }
}

impl Block {
    /// Draws this part of the transcript as plain text, with a label for any kind of block that
    /// could be mistaken for the reply.
    fn view<'a, Message: 'a>(&'a self) -> Element<'a, Message> {
        match self {
            Self::Prompt(prompt) => text(format!("> {prompt}")).into(),
            Self::Reply(words) => text(words).into(),
            Self::Reasoning(thought) => text(format!("Thinking: {thought}")).into(),
            Self::Tool { call, failed } => text(tool_summary(call, *failed))
                .wrapping(Wrapping::None)
                .into(),
            Self::Notice(words) => text(format!("Note: {words}")).into(),
        }
    }
}

/// Returns the one-line summary of a tool call, cutting long input short.
fn tool_summary(call: &ToolCallSummary, failed: bool) -> String {
    let input: String = call.input.chars().take(MAX_TOOL_INPUT_CHARS).collect();
    let cut = if input.len() < call.input.len() {
        "..."
    } else {
        ""
    };
    let failed = if failed { " (failed)" } else { "" };
    format!("Tool: {} {input}{cut}{failed}", call.name)
}

/// Describes how a Turn ended, if that needs saying.
fn turn_end_text(end: TurnEnd) -> Option<&'static str> {
    match end {
        TurnEnd::Finished => None,
        TurnEnd::Truncated => Some("the reply stopped at the model's output limit"),
        TurnEnd::Refused => Some("the model declined this request"),
        TurnEnd::Cancelled => Some("the Turn was cancelled"),
        TurnEnd::Paused => Some("the Turn paused until sstack runs again"),
    }
}
