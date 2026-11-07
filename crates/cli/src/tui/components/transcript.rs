//! The running record of the Session on screen.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};
use sstack_core::agent::{AgentEvent, TurnEnd};
use sstack_core::session::{AssistantEntry, Entry};

use crate::describe;
use crate::tui::component::{Component, KeyOutcome};
use crate::tui::event::{Event, Scroll};
use crate::tui::theme;

/// How many lines one Page Up or Page Down scrolls.
const PAGE: u16 = 10;
/// How many lines one notch of the mouse wheel scrolls. Trackpads send many small steps, so this
/// stays small.
const WHEEL: u16 = 3;

/// Shows the Session as it unfolds: what the person asked, what the model thought and replied,
/// and what its tools and the harness reported.
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
    /// same place.
    fn append(&mut self, kind: Kind, text: &str) {
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
            AgentEvent::ContextNearLimit { used, limit } => {
                self.push(Kind::Note, describe::context_warning(*used, *limit));
            }
        }
    }

    /// Shows the Turns a resumed Session already holds, as they looked when they ran.
    ///
    /// The Session does not record the model's thinking or errors that stopped a Turn, so those
    /// are missing. Tool calls left unfinished by a cancelled or cut-short Turn show as failed.
    fn resume(&mut self, entries: &[Entry]) {
        let mut tools = HashMap::new();
        for entry in entries {
            self.replay(entry, &mut tools);
        }
    }

    /// Shows one recorded entry. `tools` maps each tool call id seen so far to the tool it called,
    /// so that failures can name it.
    fn replay<'a>(&mut self, entry: &'a Entry, tools: &mut HashMap<&'a str, &'a str>) {
        match entry {
            Entry::User { text } => self.push(Kind::Prompt, text.clone()),
            Entry::Assistant(response) => {
                let calls = response.tool_calls.iter();
                tools.extend(calls.map(|call| (call.id.as_str(), call.name.as_str())));
                self.replay_response(response);
            }
            Entry::ToolResult {
                call_id,
                is_error: true,
                ..
            } => {
                let name = tools.get(call_id.as_str()).copied().unwrap_or_default();
                self.push(Kind::Note, describe::tool_failed(name));
            }
            Entry::ToolResult { .. } => {}
            Entry::TurnCancelled => self.ended(TurnEnd::Cancelled),
        }
    }

    /// Shows a recorded model response: its reply, the tools it called, and how it ended the Turn
    /// if that needs saying.
    fn replay_response(&mut self, response: &AssistantEntry) {
        if !response.text.is_empty() {
            self.append(Kind::Reply, &response.text);
        }
        for call in &response.tool_calls {
            self.push(Kind::Note, describe::tool_started(&call.into()));
        }
        self.ended(TurnEnd::from(response.stop));
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
            Event::Resumed(entries) => self.resume(entries),
            Event::TurnStarted(text) => self.push(Kind::Prompt, text.clone()),
            Event::Agent(event) => self.agent(event),
            Event::TurnEnded(outcome) => {
                if let Some(text) = describe::outcome_text(outcome.clone()) {
                    self.push(Kind::Note, text);
                }
            }
            Event::Notice(text) => self.push(Kind::Note, text.clone()),
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
    use crate::tui::test_support::{key, screen};
    use sstack_core::agent::ToolCallSummary;
    use sstack_core::session::StopReason;

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
            Event::TurnStarted("hi".into()),
            Event::Agent(AgentEvent::Reasoning("let me ".into())),
            Event::Agent(AgentEvent::Reasoning("think".into())),
            Event::Agent(AgentEvent::Text("Do".into())),
            Event::Agent(AgentEvent::Text("ne.".into())),
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
            Event::Agent(AgentEvent::ToolStarted(started)),
            Event::Agent(finished(false)),
            Event::Agent(finished(true)),
            Event::Agent(AgentEvent::ContextNearLimit { used: 8, limit: 10 }),
            Event::TurnEnded(Ok(TurnEnd::Finished)),
            Event::TurnEnded(Err("down".into())),
            Event::Notice("hello".into()),
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
        let shown = transcript(vec![Event::Resumed(vec![
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
        ])]);
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
            ]
        );
    }

    #[test]
    fn prompts_are_marked_and_set_apart() {
        let shown = transcript(vec![
            Event::Agent(AgentEvent::Text("earlier".into())),
            Event::TurnStarted("two\nlines".into()),
        ]);
        let screen = screen(20, 4, |frame| shown.render(frame, frame.area()));
        assert_eq!(screen, ["earlier", "", "> two", "lines"]);
    }

    #[test]
    fn the_newest_output_stays_in_view_unless_scrolled_back() {
        let lines: Vec<String> = (1..=30).map(|n| n.to_string()).collect();
        let mut shown = transcript(vec![Event::Agent(AgentEvent::Text(lines.join("\n")))]);
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
        let mut shown = transcript(vec![Event::Agent(AgentEvent::Text(lines.join("\n")))]);
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
