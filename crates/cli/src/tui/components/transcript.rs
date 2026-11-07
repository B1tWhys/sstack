//! The running record of the Session on screen.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};
use sstack_core::agent::AgentEvent;

use crate::describe;
use crate::tui::component::{Component, KeyOutcome};
use crate::tui::event::Event;

/// How many lines one Page Up or Page Down scrolls.
const PAGE: u16 = 10;

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
        Kind::Prompt => Style::new().bold(),
        Kind::Reply => Style::new(),
        Kind::Reasoning => Style::new().dim().italic(),
        Kind::Note => Style::new().dark_gray(),
    }
}

impl Component for Transcript {
    fn update(&mut self, event: &Event) {
        match event {
            Event::TurnStarted(text) => self.push(Kind::Prompt, text.clone()),
            Event::Agent(event) => self.agent(event),
            Event::TurnEnded(outcome) => {
                if let Some(text) = describe::outcome_text(outcome.clone()) {
                    self.push(Kind::Note, text);
                }
            }
            Event::Notice(text) => self.push(Kind::Note, text.clone()),
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        match key.code {
            KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(PAGE),
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(PAGE),
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
    use crate::tui::test_support::{key, screen};
    use sstack_core::agent::{ToolCallSummary, TurnEnd};

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
}
