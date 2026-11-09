//! What has been said in a Session, as the window shows it.
//!
//! The TUI's `Transcript` and the CLI's `describe.rs` turn the same reports into words, and parts
//! of this module look much like them, such as the joining of streamed text. By
//! `judging-duplicate-code` they are not duplicate: a change to how one interface presents a
//! Session is not a bug in the other, and the GUI is expected to present tool calls and reasoning
//! differently once it gets a visual style (ADR-0009). So the GUI keeps its own copy and its own
//! words.

mod reply;

use std::collections::HashMap;

use iced::widget::{button, column, row, text};
use iced::{Element, Font};
use sstack::agent::{AgentEvent, ToolCallSummary, TurnEnd};
use sstack::session::{AssistantEntry, Entry};
use sstack::session_report::{ApprovalRequest, SessionReport};

use crate::asked::{Asked, Asking, Controls};

pub use self::reply::Reply;

/// Space between the parts of the transcript, in logical pixels.
const SPACING: f32 = 10.0;

/// The Session as it unfolds in the window: what the Session held when it opened, what the
/// person sent, what the model thought and replied, the tools it called and the questions about
/// them, the model's Questions, and what the window tells the person.
#[derive(Debug, Default)]
pub struct Transcript {
    /// Everything shown, oldest first.
    blocks: Vec<Block>,
}

/// What pressing things in the transcript sends to the window.
#[derive(Debug, Clone)]
pub struct Handlers<Message> {
    /// Sent when the person answers an open question about a shell command, saying whether it
    /// may run.
    pub answer: fn(bool) -> Message,
    /// Sent when the person clicks a link in a reply, with the link's address.
    pub open: fn(String) -> Message,
    /// Sent by the controls of the model's open Questions.
    pub questions: Controls<Message>,
}

/// One part of the transcript.
#[derive(Debug, PartialEq, Eq)]
pub enum Block {
    /// What the person sent.
    Prompt(String),
    /// The model's reply in one Step, as much of it as has arrived.
    Reply(Reply),
    /// The model's summarized thinking in one Step, as much of it as has arrived.
    Reasoning(String),
    /// A tool call the model made.
    Tool {
        /// Which tool, with what input.
        call: ToolCallSummary,
        /// Whether the call failed. A call still running has not failed.
        failed: bool,
        /// The question whether the call's command may run, if the person was asked.
        approval: Option<Approval>,
    },
    /// A set of the model's Questions, and what became of them.
    Questions(Asked),
    /// Something the window tells the person, such as how a Turn ended or why it could not do
    /// what they asked.
    Notice(String),
}

/// A question to the person whether a command may run, and what became of it.
///
/// Once answered it shows as a word on its tool call's line, but the transcript keeps the whole
/// command and note, so that a later change can show them again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// The command, exactly as it would run.
    pub command: String,
    /// Why the person is asked, if the asker said.
    pub note: Option<String>,
    /// Where the question stands.
    pub decision: Decision,
}

/// Where a question whether a command may run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The question waits for the person.
    Open,
    /// The person let the command run.
    Allowed,
    /// The person refused the command.
    Denied,
    /// The Turn ended before the person answered, which refuses the command.
    Unanswered,
    /// A Rebuild put the question off and the Turn parked. The Turn asks again, on a new tool
    /// call, when it carries on.
    PutOff,
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
            SessionReport::Approval(request) => self.ask(request),
            SessionReport::Question(request) => {
                let asked = Asked::new(request.questions.clone());
                self.blocks.push(Block::Questions(asked));
                true
            }
            _ => false,
        }
    }

    /// Notes the person's answer to the oldest open question about a command: `allowed` says
    /// whether it may run.
    pub fn answered(&mut self, allowed: bool) {
        let decision = if allowed {
            Decision::Allowed
        } else {
            Decision::Denied
        };
        if let Some(approval) = self.open_approvals().next() {
            approval.decision = decision;
        }
    }

    /// Returns the oldest set of the model's Questions still open, to work on or settle.
    pub fn open_questions(&mut self) -> Option<&mut Asked> {
        self.open_asked().next()
    }

    /// Returns every set of the model's Questions still open, oldest first.
    fn open_asked(&mut self) -> impl Iterator<Item = &mut Asked> {
        self.blocks.iter_mut().filter_map(|block| match block {
            Block::Questions(asked) if asked.state == Asking::Open => Some(asked),
            _ => None,
        })
    }

    /// Returns everything shown, oldest first.
    #[cfg(test)]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Draws the transcript, one block under another, with the model's replies formatted as
    /// Markdown. What the person presses in it sends what `handlers` says.
    pub fn view<'a, Message: Clone + 'a>(
        &'a self,
        handlers: &Handlers<Message>,
    ) -> Element<'a, Message> {
        column(self.blocks.iter().map(|block| block.view(handlers)))
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
            self.blocks.push(Block::Reply(piece.into()));
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
            approval: None,
        });
    }

    /// Puts `request` to the person on the newest tool call, which is the one asking, returning
    /// whether there was a call to put it on. Tools run one at a time, and each is shown before
    /// it runs.
    fn ask(&mut self, request: &ApprovalRequest) -> bool {
        let newest = self.blocks.iter_mut().rev().find_map(|block| match block {
            Block::Tool { approval, .. } => Some(approval),
            _ => None,
        });
        let approval = Approval {
            command: request.command.clone(),
            note: request.note.clone(),
            decision: Decision::Open,
        };
        newest.map(|asked| *asked = Some(approval)).is_some()
    }

    /// Returns the questions about commands that still wait for the person, oldest first.
    fn open_approvals(&mut self) -> impl Iterator<Item = &mut Approval> {
        self.blocks.iter_mut().filter_map(|block| match block {
            Block::Tool {
                approval: Some(approval),
                ..
            } if approval.decision == Decision::Open => Some(approval),
            _ => None,
        })
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

    /// Notes how a Turn ended or the error that stopped it, returning whether the transcript
    /// changed. Questions the Turn left open, of either kind, are marked unanswered, or put off
    /// if it parked.
    fn ended(&mut self, outcome: Result<TurnEnd, &String>) -> bool {
        let lapsed_as = if outcome == Ok(TurnEnd::Paused) {
            Decision::PutOff
        } else {
            Decision::Unanswered
        };
        let mut lapsed = false;
        for approval in self.open_approvals() {
            approval.decision = lapsed_as;
            lapsed = true;
        }
        for asked in self.open_asked() {
            asked.state = Asking::Unanswered;
            lapsed = true;
        }
        let words = match outcome {
            Ok(end) => turn_end_text(end).map(str::to_owned),
            Err(error) => Some(format!("the Turn failed: {error}")),
        };
        words.map(|words| self.notice(words)).is_some() || lapsed
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
    /// Draws this part of the transcript: a reply formatted as Markdown, and everything else as
    /// plain text, with a label for any kind of block that could be mistaken for the reply. What
    /// the person presses in it sends what `handlers` says.
    fn view<'a, Message: Clone + 'a>(
        &'a self,
        handlers: &Handlers<Message>,
    ) -> Element<'a, Message> {
        match self {
            Self::Prompt(prompt) => text(format!("> {prompt}")).into(),
            Self::Reply(reply) => reply.view(handlers.open),
            Self::Reasoning(thought) => text(format!("Thinking: {thought}")).into(),
            Self::Tool {
                call,
                failed,
                approval,
            } => {
                let line = tool_summary(call, *failed, approval.as_ref());
                tool_view(line, approval.as_ref(), handlers.answer)
            }
            Self::Questions(asked) => asked.view(&handlers.questions),
            Self::Notice(words) => text(format!("Note: {words}")).into(),
        }
    }
}

impl Approval {
    /// Draws the open question: why the person is asked, the whole command, and the buttons
    /// that send what `answer` makes of whether it may run.
    fn view<'a, Message: Clone + 'a>(
        &'a self,
        answer: fn(bool) -> Message,
    ) -> Element<'a, Message> {
        let answers = row![
            button("Allow").on_press(answer(true)),
            button("Deny").on_press(answer(false)),
        ]
        .spacing(SPACING);
        column![]
            .push(self.note.as_deref().map(text))
            .push(text(self.command.as_str()).font(Font::MONOSPACE))
            .push(answers)
            .spacing(SPACING)
            .into()
    }
}

impl Decision {
    /// Returns the word that marks an answered question on its tool call's line. An open
    /// question has none, since it shows in full.
    fn label(self) -> Option<&'static str> {
        match self {
            Self::Open => None,
            Self::Allowed => Some("allowed"),
            Self::Denied => Some("denied"),
            Self::Unanswered => Some("not answered"),
            Self::PutOff => Some("put off"),
        }
    }
}

/// Draws a tool call as its summary `line`, wrapped to fit, with the question about it in full
/// below while it is open. Pressing Allow or Deny sends what `answer` makes of whether the command
/// may run.
fn tool_view<'a, Message: Clone + 'a>(
    line: String,
    approval: Option<&'a Approval>,
    answer: fn(bool) -> Message,
) -> Element<'a, Message> {
    let line = text(line);
    match approval {
        Some(open) if open.decision == Decision::Open => {
            column![line, open.view(answer)].spacing(SPACING).into()
        }
        _ => line.into(),
    }
}

/// Returns the summary of a tool call, with its whole input, marking how any question about it
/// was answered and whether it failed.
fn tool_summary(call: &ToolCallSummary, failed: bool, approval: Option<&Approval>) -> String {
    let marks = marks(failed, approval.map(|approval| approval.decision));
    format!("Tool: {} {}{marks}", call.name, call.input)
}

/// Returns what goes after a tool call's summary to say how any question about it was answered
/// and whether it failed, such as ` (denied, failed)`, or nothing if neither needs saying.
fn marks(failed: bool, decision: Option<Decision>) -> String {
    let words: Vec<&str> = decision
        .and_then(Decision::label)
        .into_iter()
        .chain(failed.then_some("failed"))
        .collect();
    if words.is_empty() {
        String::new()
    } else {
        format!(" ({})", words.join(", "))
    }
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
