//! The running record of the Session on screen.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};
use sstack::agent::{AgentEvent, TurnEnd};

use crate::describe;
use crate::tui::component::{Component, KeyOutcome};
use sstack::session_report::{Replayed, SessionReport};

use crate::tui::event::{Event, Scroll};
use crate::tui::theme;

/// How many lines one Page Up or Page Down scrolls.
const PAGE: u16 = 10;
/// How many lines one notch of the mouse wheel scrolls. Trackpads send many small steps, so this
/// stays small.
const WHEEL: u16 = 3;

/// The Session as it unfolds: what the person asked, what the model thought and replied, and
/// what its tools and the harness reported.
///
/// It follows new output unless the person has scrolled back with Page Up.
#[derive(Debug, Default)]
pub struct Transcript {
    /// Everything shown so far, oldest first.
    blocks: Vec<Block>,
    /// How many lines the view sits above the newest output. Zero follows new output.
    scroll_back: u16,
}

/// One piece of the transcript, which starts on a new line.
#[derive(Debug, PartialEq, Eq)]
struct Block {
    /// Who or what the text came from, which decides its look.
    kind: Kind,
    /// The text, which may span several lines.
    text: String,
}

/// Where a piece of the transcript came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Input from the person.
    Prompt,
    /// The model's reply.
    Reply,
    /// The model's summarized thinking.
    Reasoning,
    /// Tool activity, warnings, and messages from the harness itself.
    Note,
}

impl Transcript {
    /// Adds text that streams in pieces, joining it to the previous piece if that came from the
    /// same place. Empty text adds nothing, so a response with no words before its tool calls
    /// leaves no blank line, just as it does when the Session is shown again.
    fn append(&mut self, kind: Kind, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.blocks.last_mut() {
            Some(last) if last.kind == kind => last.text.push_str(text),
            _ => self.push(kind, text.to_owned()),
        }
    }

    /// Adds text that stands on its own.
    fn push(&mut self, kind: Kind, text: String) {
        self.blocks.push(Block { kind, text });
    }

    /// Records something that happened during a Turn.
    fn agent(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Text(text) => self.append(Kind::Reply, text),
            AgentEvent::Reasoning(text) => self.append(Kind::Reasoning, text),
            AgentEvent::ToolStarted(call) => self.push(Kind::Note, describe::tool_started(call)),
            AgentEvent::ToolFinished {
                name,
                is_error: true,
            } => self.push(Kind::Note, describe::tool_failed(name)),
            AgentEvent::ToolFinished { .. } => {}
            AgentEvent::Admitted(text) => self.push(Kind::Prompt, text.clone()),
            AgentEvent::ContextNearLimit { used, limit } => {
                self.push(Kind::Note, describe::context_warning(*used, *limit));
            }
        }
    }

    /// Shows the Turns a resumed Session already holds, as they looked when they ran.
    ///
    /// The Session does not record the model's thinking or errors that stopped a Turn, so those
    /// are missing. Tool calls left unfinished by a cancelled or cut-short Turn show as failed.
    /// A Turn that a Rebuild put off shows as it was before, and its stop says nothing.
    fn resume(&mut self, shown: &[Replayed]) {
        let mut tools = HashMap::new();
        for replayed in shown {
            self.replay(replayed, &mut tools);
        }
    }

    /// Shows one thing a resumed Session holds. `tools` holds the tool each call id shown so far
    /// called, so that failures can name it.
    fn replay<'a>(&mut self, replayed: &'a Replayed, tools: &mut HashMap<&'a str, &'a str>) {
        match replayed {
            Replayed::Prompt(text) => self.push(Kind::Prompt, text.clone()),
            Replayed::Reply(text) => self.append(Kind::Reply, text),
            Replayed::ToolStarted { id, call } => {
                tools.insert(id, &call.name);
                self.push(Kind::Note, describe::tool_started(call));
            }
            Replayed::ToolFinished { id, is_error: true } => {
                let name = tools.get(id.as_str()).copied().unwrap_or_default();
                self.push(Kind::Note, describe::tool_failed(name));
            }
            Replayed::ToolFinished { .. } => {}
            Replayed::TurnEnded(end) => self.ended(*end),
        }
    }

    /// Notes how a Turn ended, if that needs saying.
    fn ended(&mut self, end: TurnEnd) {
        if let Some(text) = describe::turn_end_text(end) {
            self.push(Kind::Note, text.to_owned());
        }
    }

    /// Moves the view `lines` towards older or newer output. Scrolling down to the newest output
    /// follows it again.
    fn scroll(&mut self, direction: Scroll, lines: u16) {
        self.scroll_back = match direction {
            Scroll::Up => self.scroll_back.saturating_add(lines),
            Scroll::Down => self.scroll_back.saturating_sub(lines),
        };
    }

    /// Returns everything shown so far, styled.
    fn text(&self) -> Text<'_> {
        self.blocks.iter().flat_map(Block::lines).collect()
    }
}

impl Block {
    /// Returns the block's lines with the look for where it came from. Prompts get a blank line
    /// above, so each Turn stands apart.
    fn lines(&self) -> Vec<Line<'_>> {
        let style = style(self.kind);
        let gap = (self.kind == Kind::Prompt).then(Line::default);
        let prefix = if self.kind == Kind::Prompt { "> " } else { "" };
        let body = self.text.split('\n').enumerate().map(move |(index, line)| {
            let lead = if index == 0 { prefix } else { "" };
            Line::styled(format!("{lead}{line}"), style)
        });
        gap.into_iter().chain(body).collect()
    }
}

/// Returns the look for text from `kind`.
fn style(kind: Kind) -> Style {
    match kind {
        Kind::Prompt => theme::PROMPT,
        Kind::Reply => theme::TEXT,
        Kind::Reasoning => theme::REASONING,
        Kind::Note => theme::NOTE,
    }
}

impl Component for Transcript {
    fn update(&mut self, event: &Event) {
        match event {
            Event::Report(SessionReport::Resumed(entries)) => self.resume(entries),
            Event::Report(SessionReport::TurnStarted(text)) => {
                self.push(Kind::Prompt, text.clone());
            }
            Event::Report(SessionReport::Agent(event)) => self.agent(event),
            Event::Report(SessionReport::TurnEnded(outcome)) => {
                if let Some(text) = describe::outcome_text(outcome.clone()) {
                    self.push(Kind::Note, text);
                }
            }
            Event::Report(SessionReport::Notice(text)) => self.push(Kind::Note, text.clone()),
            Event::Scroll(direction) => self.scroll(*direction, WHEEL),
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        match key.code {
            KeyCode::PageUp => self.scroll(Scroll::Up, PAGE),
            KeyCode::PageDown => self.scroll(Scroll::Down, PAGE),
            _ => return KeyOutcome::Ignored,
        }
        KeyOutcome::Handled
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let paragraph = Paragraph::new(self.text()).wrap(Wrap { trim: false });
        let lines = u16::try_from(paragraph.line_count(area.width)).unwrap_or(u16::MAX);
        let top = lines
            .saturating_sub(area.height)
            .saturating_sub(self.scroll_back);
        frame.render_widget(paragraph.scroll((top, 0)), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::{bash_call, reply};
    use crate::tui::test_support::{drawn, key, screen};
    use insta::assert_snapshot;
    use sstack::agent::ToolCallSummary;
    use sstack::agent::replay;
    use sstack::session::{AssistantEntry, Entry, StopReason};

    fn transcript(events: Vec<Event>) -> Transcript {
        let mut transcript = Transcript::default();
        for event in events {
            transcript.update(&event);
        }
        transcript
    }

    fn texts(transcript: &Transcript) -> Vec<(Kind, &str)> {
        let blocks = transcript.blocks.iter();
        blocks
            .map(|block| (block.kind, block.text.as_str()))
            .collect()
    }

    #[test]
    fn streamed_pieces_join_until_their_source_changes() {
        let shown = transcript(vec![
            Event::Report(SessionReport::TurnStarted("hi".into())),
            Event::Report(SessionReport::Agent(AgentEvent::Reasoning(
                "let me ".into(),
            ))),
            Event::Report(SessionReport::Agent(AgentEvent::Reasoning("think".into()))),
            Event::Report(SessionReport::Agent(AgentEvent::Text("Do".into()))),
            Event::Report(SessionReport::Agent(AgentEvent::Text("ne.".into()))),
        ]);
        assert_eq!(
            texts(&shown),
            [
                (Kind::Prompt, "hi"),
                (Kind::Reasoning, "let me think"),
                (Kind::Reply, "Done."),
            ]
        );
    }

    #[test]
    fn tools_warnings_outcomes_and_notices_become_notes() {
        let started = ToolCallSummary {
            name: "bash".into(),
            input: "ls".into(),
        };
        let finished = |is_error| AgentEvent::ToolFinished {
            name: "bash".into(),
            is_error,
        };
        let shown = transcript(vec![
            Event::Report(SessionReport::Agent(AgentEvent::ToolStarted(started))),
            Event::Report(SessionReport::Agent(finished(false))),
            Event::Report(SessionReport::Agent(finished(true))),
            Event::Report(SessionReport::Agent(AgentEvent::ContextNearLimit {
                used: 8,
                limit: 10,
            })),
            Event::Report(SessionReport::TurnEnded(Ok(TurnEnd::Finished))),
            Event::Report(SessionReport::TurnEnded(Err("down".into()))),
            Event::Report(SessionReport::Notice("hello".into())),
            Event::Resize,
        ]);
        let notes: Vec<_> = texts(&shown).into_iter().map(|(_, text)| text).collect();
        assert_eq!(notes[..2], ["[bash] ls", "[bash] failed"]);
        assert!(notes[2].contains("8 of 10 tokens"));
        assert_eq!(notes[3..], ["error: down", "hello"]);
    }

    #[test]
    fn a_resumed_session_shows_its_earlier_turns_as_they_ran() {
        let result = |is_error| Entry::ToolResult {
            call_id: "1".into(),
            output: "out".into(),
            is_error,
        };
        let shown = transcript(vec![Event::Report(SessionReport::Resumed(replay(&[
            Entry::User {
                text: "fix it".into(),
            },
            Entry::Assistant(bash_call("ls")),
            result(false),
            Entry::Assistant(AssistantEntry {
                text: "Trying again.".into(),
                ..bash_call("false")
            }),
            result(true),
            Entry::Assistant(AssistantEntry {
                stop: StopReason::MaxTokens,
                ..reply("cut")
            }),
            Entry::User {
                text: "next".into(),
            },
            Entry::TurnCancelled,
            Entry::TurnParked,
        ])))]);
        assert_eq!(
            texts(&shown),
            [
                (Kind::Prompt, "fix it"),
                (Kind::Note, r#"[bash] {"command":"ls"}"#),
                (Kind::Reply, "Trying again."),
                (Kind::Note, r#"[bash] {"command":"false"}"#),
                (Kind::Note, "[bash] failed"),
                (Kind::Reply, "cut"),
                (Kind::Note, "[stopped: the response hit the output limit]"),
                (Kind::Prompt, "next"),
                (Kind::Note, "[cancelled]"),
                (Kind::Note, "[paused until the harness runs again]"),
            ]
        );
    }

    #[test]
    fn prompts_are_marked_and_set_apart() {
        let shown = transcript(vec![
            Event::Report(SessionReport::Agent(AgentEvent::Text("earlier".into()))),
            Event::Report(SessionReport::TurnStarted("two\nlines".into())),
        ]);
        let screen = drawn(20, 4, |frame| shown.render(frame, frame.area()));
        assert_snapshot!(screen.backend());
    }

    #[test]
    fn the_newest_output_stays_in_view_unless_scrolled_back() {
        let lines: Vec<String> = (1..=30).map(|n| n.to_string()).collect();
        let mut shown = transcript(vec![Event::Report(SessionReport::Agent(AgentEvent::Text(
            lines.join("\n"),
        )))]);
        let draw = |shown: &Transcript| screen(5, 2, |frame| shown.render(frame, frame.area()));
        assert_eq!(draw(&shown), ["29", "30"]);

        assert_eq!(shown.key(key(KeyCode::PageUp)), KeyOutcome::Handled);
        assert_eq!(draw(&shown), ["19", "20"]);

        assert_eq!(shown.key(key(KeyCode::PageDown)), KeyOutcome::Handled);
        assert_eq!(shown.key(key(KeyCode::PageDown)), KeyOutcome::Handled);
        assert_eq!(draw(&shown), ["29", "30"]);

        assert_eq!(shown.key(key(KeyCode::Enter)), KeyOutcome::Ignored);
    }

    #[test]
    fn the_mouse_wheel_scrolls_a_few_lines_at_a_time() {
        let lines: Vec<String> = (1..=30).map(|n| n.to_string()).collect();
        let mut shown = transcript(vec![Event::Report(SessionReport::Agent(AgentEvent::Text(
            lines.join("\n"),
        )))]);
        let draw = |shown: &Transcript| screen(5, 2, |frame| shown.render(frame, frame.area()));

        shown.update(&Event::Scroll(Scroll::Up));
        shown.update(&Event::Scroll(Scroll::Up));
        assert_eq!(draw(&shown), ["23", "24"]);

        shown.update(&Event::Scroll(Scroll::Down));
        assert_eq!(draw(&shown), ["26", "27"]);
        shown.update(&Event::Scroll(Scroll::Down));
        shown.update(&Event::Scroll(Scroll::Down));
        assert_eq!(draw(&shown), ["29", "30"]);
    }
}
