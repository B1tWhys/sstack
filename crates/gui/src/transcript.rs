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

use iced::widget::{Space, Text, button, column, container, hover, row, text};
use iced::{Element, Fill, Font};
use lucide_icons::Icon;
use sstack::protocol::{
    AgentEvent, ApprovalRequest, Question, Replayed, SessionReport, ToolCallSummary, TurnEnd,
};

use crate::asked::{Asked, Asking, Controls};
use crate::hint::hint;
use crate::theme;

pub use self::reply::Reply;

/// Space between the parts of the transcript, and around what the person said inside its panel,
/// in logical pixels.
const SPACING: f32 = 10.0;

/// The size of the model's answers, in logical pixels, which is iced's default text size.
const TEXT_SIZE: f32 = 16.0;

/// The size of text that matters less than the answers around it, such as what the model thought,
/// in logical pixels.
const ASIDE_SIZE: f32 = 14.0;

/// The size of the copy button's icon, in logical pixels, small enough that the button fits
/// beside a single line of text.
const COPY_ICON_SIZE: f32 = 14.0;

/// The room around the copy button's icon, in logical pixels.
const COPY_PADDING: f32 = 1.0;

/// How wide the room kept beside every part of the transcript for its copy button is, in logical
/// pixels, so that the button never covers the text.
const COPY_WIDTH: f32 = 20.0;

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
    /// Sent when the person presses the copy button beside a part of the transcript, with the
    /// text to put on the clipboard.
    pub copy: fn(String) -> Message,
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
                self.questions(request.questions.clone());
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

    /// Puts `questions` to the person, in the place they had if a Rebuild put them off and the
    /// Turn now asks them again.
    fn questions(&mut self, questions: Vec<Question>) {
        let put_off = self.blocks.iter_mut().find_map(|block| match block {
            Block::Questions(asked) if asked.state == Asking::PutOff => Some(asked),
            _ => None,
        });
        match put_off {
            Some(asked) => asked.state = Asking::Open,
            None => self.blocks.push(Block::Questions(Asked::new(questions))),
        }
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
    /// Markdown. What the person said stands in a panel, and what the model thought or said on its
    /// way to an answer is smaller and muted, so that its answers stand out. A copy button shows
    /// beside each block the pointer is over, except the model's Questions. What the person
    /// presses in it sends what `handlers` says.
    pub fn view<'a, Message: Clone + 'a>(
        &'a self,
        handlers: &Handlers<Message>,
    ) -> Element<'a, Message> {
        let drawn = self.blocks.iter().enumerate().map(|(index, block)| {
            let drawn = match block {
                Block::Reply(reply) if self.interim(index) => reply.aside(handlers.open),
                _ => block.view(handlers),
            };
            copyable(drawn, block.copied(), handlers.copy)
        });
        column(drawn).spacing(SPACING).into()
    }

    /// Returns whether the reply at `index` was said on the way to an answer rather than being
    /// one, because the model called a tool after it in the same Step. A reply still arriving
    /// counts as an answer until a tool call starts.
    pub fn interim(&self, index: usize) -> bool {
        let after = self.blocks.get(index + 1..).unwrap_or_default();
        // Thinking and notices can come between a Step's reply and its tool calls, or between
        // Steps, so they say nothing either way.
        let next = after
            .iter()
            .find(|block| !matches!(block, Block::Reasoning(_) | Block::Notice(_)));
        matches!(next, Some(Block::Tool { .. }))
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
            AgentEvent::Admitted(text) => self.prompt(text.clone()),
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
    /// changed. Questions the Turn left open, of either kind, are marked unanswered. If a
    /// Rebuild put them off instead, they show as they did before they were asked, until the
    /// Turn asks them again.
    fn ended(&mut self, outcome: Result<TurnEnd, &String>) -> bool {
        let lapsed = if outcome == Ok(TurnEnd::PutOff) {
            self.put_off_open()
        } else {
            self.lapse_open()
        };
        let words = match outcome {
            Ok(end) => turn_end_text(end).map(str::to_owned),
            Err(error) => Some(format!("the Turn failed: {error}")),
        };
        words.map(|words| self.notice(words)).is_some() || lapsed
    }

    /// Marks every question still open, of either kind, as unanswered, returning whether there
    /// was one.
    fn lapse_open(&mut self) -> bool {
        let mut lapsed = false;
        for approval in self.open_approvals() {
            approval.decision = Decision::Unanswered;
            lapsed = true;
        }
        for asked in self.open_asked() {
            asked.state = Asking::Unanswered;
            lapsed = true;
        }
        lapsed
    }

    /// Hides every question still open, of either kind, because a Rebuild put it off, returning
    /// whether there was one. A tool call goes back to its line alone, and the model's Questions
    /// keep what the person had chosen, in case the Turn asks them again in this run.
    fn put_off_open(&mut self) -> bool {
        let mut put_off = false;
        for block in &mut self.blocks {
            match block {
                Block::Tool { approval, .. }
                    if approval
                        .as_ref()
                        .is_some_and(|open| open.decision == Decision::Open) =>
                {
                    *approval = None;
                }
                Block::Questions(asked) if asked.state == Asking::Open => {
                    asked.state = Asking::PutOff;
                }
                _ => continue,
            }
            put_off = true;
        }
        put_off
    }

    /// Shows the Turns the Session already held, before everything already shown, returning
    /// whether it held any.
    ///
    /// The Session does not record the model's thinking or errors that stopped a Turn, so those
    /// are missing. Tool calls left unfinished by a cancelled Turn show as failed. A Turn that a
    /// Rebuild put off shows as it was before.
    fn resume(&mut self, shown: &[Replayed]) -> bool {
        let mut earlier = Self::default();
        let mut calls = HashMap::new();
        for replayed in shown {
            earlier.replay(replayed, &mut calls);
        }
        self.blocks.splice(0..0, earlier.blocks);
        !shown.is_empty()
    }

    /// Shows one thing a resumed Session holds. `calls` holds where the block of each tool call
    /// shown so far is, so that its result can mark it failed.
    fn replay<'a>(&mut self, replayed: &'a Replayed, calls: &mut HashMap<&'a str, usize>) {
        match replayed {
            Replayed::Prompt(text) => self.prompt(text.clone()),
            Replayed::Reply(text) => {
                self.reply(text);
            }
            Replayed::ToolStarted { id, call } => {
                calls.insert(id, self.blocks.len());
                self.tool(call.clone());
            }
            Replayed::ToolFinished { id, is_error } => {
                self.replay_result(calls.get(id.as_str()), *is_error);
            }
            Replayed::TurnEnded(end) => self.replay_end(*end),
        }
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
    /// Draws this part of the transcript: a reply as an answer formatted as Markdown, what the
    /// person said in a panel, thinking muted, and everything else as plain text, with a label for
    /// any kind of block that could be mistaken for a reply. What the person presses in it sends
    /// what `handlers` says.
    fn view<'a, Message: Clone + 'a>(
        &'a self,
        handlers: &Handlers<Message>,
    ) -> Element<'a, Message> {
        match self {
            Self::Prompt(prompt) => container(text(prompt))
                .padding(SPACING)
                .width(Fill)
                .style(theme::panel)
                .into(),
            Self::Reply(reply) => reply.view(handlers.open),
            Self::Reasoning(thought) => aside(format!("Thinking: {thought}")),
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

impl Block {
    /// Returns the text that copying this part of the transcript puts on the clipboard, without
    /// the label it is drawn with: a reply as the Markdown the model sent, and a tool call as its
    /// input. The model's Questions have none, since they cannot be copied.
    pub fn copied(&self) -> Option<&str> {
        match self {
            Self::Prompt(words) | Self::Reasoning(words) | Self::Notice(words) => Some(words),
            Self::Reply(reply) => Some(reply.source()),
            Self::Tool { call, .. } => Some(&call.input),
            Self::Questions(_) => None,
        }
    }
}

/// Draws `drawn` with room beside it for a copy button, which shows while the pointer is over it
/// and sends what `copy` makes of `words`. Without `words` the room stays empty, so that every
/// part of the transcript lines up.
fn copyable<'a, Message: Clone + 'a>(
    drawn: Element<'a, Message>,
    words: Option<&'a str>,
    copy: fn(String) -> Message,
) -> Element<'a, Message> {
    let base = row![container(drawn).width(Fill), Space::new().width(COPY_WIDTH)].width(Fill);
    let Some(words) = words else {
        return base.into();
    };
    let icon = Text::from(Icon::Copy).size(COPY_ICON_SIZE);
    let press = button(icon)
        .padding(COPY_PADDING)
        .style(button::text)
        .on_press_with(move || copy(words.to_owned()));
    hover(base, container(hint(press, "Copy")).align_right(Fill))
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
        }
    }
}

/// Draws `words` as an aside, smaller and dimmer than the answers around it.
fn aside<'a, Message: 'a>(words: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(words).size(ASIDE_SIZE).style(theme::muted).into()
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
        // A Turn put off by a Rebuild comes back as it was, so its stop shows nothing.
        TurnEnd::Finished | TurnEnd::PutOff => None,
        TurnEnd::Truncated => Some("the reply stopped at the model's output limit"),
        TurnEnd::Refused => Some("the model declined this request"),
        TurnEnd::Cancelled => Some("the Turn was cancelled"),
        TurnEnd::Paused => Some("the Turn paused until sstack runs again"),
    }
}

#[cfg(test)]
mod tests {
    use sstack::agent::replay;
    use sstack::protocol::AgentEvent;
    use sstack::session::{AssistantEntry, Entry};

    use super::*;
    use crate::test_support::{bash_call, reply};

    /// Returns, for each reply in `transcript` in order, whether it was said on the way to an
    /// answer.
    fn interim_replies(transcript: &Transcript) -> Vec<bool> {
        let blocks = transcript.blocks();
        (0..blocks.len())
            .filter(|index| matches!(blocks[*index], Block::Reply(_)))
            .map(|index| transcript.interim(index))
            .collect()
    }

    /// Shows `events` in `transcript` as they happen during a Turn.
    fn happened(transcript: &mut Transcript, events: impl IntoIterator<Item = AgentEvent>) {
        for event in events {
            transcript.report(&SessionReport::Agent(event));
        }
    }

    #[test]
    fn a_reply_followed_by_a_tool_call_is_interim_and_the_last_reply_is_the_answer() {
        let mut transcript = Transcript::default();
        transcript.prompt("look".into());
        let read = ToolCallSummary {
            name: "read".into(),
            input: "{}".into(),
        };
        happened(
            &mut transcript,
            [
                AgentEvent::Text("Looking.".into()),
                AgentEvent::ToolStarted(read),
                AgentEvent::ToolFinished {
                    name: "read".into(),
                    is_error: false,
                },
                AgentEvent::Reasoning("found".into()),
                AgentEvent::Text("Found it.".into()),
            ],
        );
        assert_eq!(interim_replies(&transcript), [true, false]);
    }

    #[test]
    fn an_answer_stays_one_when_the_next_turn_starts_with_a_tool_call() {
        let looking = AssistantEntry {
            text: "Looking again.".into(),
            ..bash_call("ls")
        };
        let held = [
            Entry::User { text: "one".into() },
            Entry::Assistant(reply("Done.")),
            Entry::User { text: "two".into() },
            Entry::Assistant(bash_call("ls")),
            Entry::Assistant(looking),
            Entry::Assistant(reply("Still done.")),
        ];
        let mut transcript = Transcript::default();
        transcript.report(&SessionReport::Resumed(replay(&held)));
        assert_eq!(interim_replies(&transcript), [false, true, false]);
    }

    #[test]
    fn copying_a_block_takes_its_text_without_its_label() {
        let tool = Block::Tool {
            call: ToolCallSummary {
                name: "bash".into(),
                input: r#"{"command":"ls"}"#.into(),
            },
            failed: true,
            approval: None,
        };
        let blocks = [
            Block::Prompt("look".into()),
            Block::Reply("**bold**".into()),
            Block::Reasoning("hmm".into()),
            tool,
            Block::Notice("down".into()),
            Block::Questions(Asked::new(vec![])),
        ];

        let copied: Vec<_> = blocks.iter().map(Block::copied).collect();

        let expected = [
            Some("look"),
            Some("**bold**"),
            Some("hmm"),
            Some(r#"{"command":"ls"}"#),
            Some("down"),
            None,
        ];
        assert_eq!(copied, expected);
    }
}
