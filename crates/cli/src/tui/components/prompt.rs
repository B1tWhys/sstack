//! Where the person types.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::tui::component::{Component, KeyOutcome};
use crate::tui::event::{Action, Event};

/// Shown before what the person types.
const MARKER: &str = "> ";

/// Collects a line from the person and submits it when they press Enter.
///
/// It submits whatever is typed, leaving the App to decide whether that can happen now.
#[derive(Debug, Default)]
pub struct Prompt {
    /// What has been typed since the last submission.
    line: String,
}

impl Component for Prompt {
    fn update(&mut self, event: &Event) {
        if let Event::Paste(text) = event {
            self.line.push_str(text);
        }
    }

    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        let typed = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(character) if typed => self.line.push(character),
            KeyCode::Backspace => _ = self.line.pop(),
            KeyCode::Enter => {
                return KeyOutcome::Act(Action::Submit(std::mem::take(&mut self.line)));
            }
            _ => return KeyOutcome::Ignored,
        }
        KeyOutcome::Handled
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let line = Line::from(format!("{MARKER}{}", self.line));
        let cursor = u16::try_from(line.width()).unwrap_or(u16::MAX);
        let block = Block::bordered();
        let inner = block.inner(area);
        let paragraph = Paragraph::new(line).wrap(Wrap { trim: false });
        frame.render_widget(paragraph.block(block), area);
        frame.set_cursor_position((inner.x.saturating_add(cursor).min(inner.right()), inner.y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::{ctrl, key, screen, type_into};

    #[test]
    fn typed_and_pasted_text_is_submitted_on_enter_and_then_cleared() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "helo");
        assert_eq!(prompt.key(key(KeyCode::Backspace)), KeyOutcome::Handled);
        prompt.update(&Event::Paste("lo\nthere".into()));

        let submitted = prompt.key(key(KeyCode::Enter));

        assert_eq!(
            submitted,
            KeyOutcome::Act(Action::Submit("hello\nthere".into()))
        );
        assert_eq!(prompt.line, "");
    }

    #[test]
    fn shortcuts_and_unknown_keys_pass_through() {
        let mut prompt = Prompt::default();
        assert_eq!(prompt.key(ctrl('a')), KeyOutcome::Ignored);
        assert_eq!(prompt.key(key(KeyCode::PageUp)), KeyOutcome::Ignored);
        assert_eq!(prompt.line, "");
    }

    #[test]
    fn what_is_typed_is_shown_after_a_marker() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "hi");
        let shown = screen(10, 3, |frame| prompt.render(frame, frame.area()));
        assert_eq!(shown[1], "│> hi    │");
    }
}
