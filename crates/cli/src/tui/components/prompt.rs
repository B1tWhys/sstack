//! Where the person types.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use sstack_core::vim::Draft;

use super::wrap::{BORDER, GUTTER, gutter, normalize_newlines, to_u16, wrap};
use crate::tui::component::{Component, KeyOutcome};
use crate::tui::editor::{Edit, Editor};
use crate::tui::event::{Action, Event};
use crate::tui::theme;

/// The keys that edit what is typed, in the style of a shell's command line, and the edit each
/// one makes.
///
/// A key matches only with exactly these modifiers held. Alt also covers Option on macOS
/// terminals set to send it as Meta.
const BINDINGS: &[(KeyModifiers, KeyCode, Edit)] = &[
    (KeyModifiers::NONE, KeyCode::Left, Editor::move_left),
    (KeyModifiers::CONTROL, KeyCode::Char('b'), Editor::move_left),
    (KeyModifiers::NONE, KeyCode::Right, Editor::move_right),
    (
        KeyModifiers::CONTROL,
        KeyCode::Char('f'),
        Editor::move_right,
    ),
    (KeyModifiers::NONE, KeyCode::Home, Editor::move_line_start),
    (
        KeyModifiers::CONTROL,
        KeyCode::Char('a'),
        Editor::move_line_start,
    ),
    (KeyModifiers::NONE, KeyCode::End, Editor::move_line_end),
    (
        KeyModifiers::CONTROL,
        KeyCode::Char('e'),
        Editor::move_line_end,
    ),
    (
        KeyModifiers::ALT,
        KeyCode::Char('b'),
        Editor::move_word_left,
    ),
    (KeyModifiers::ALT, KeyCode::Left, Editor::move_word_left),
    (KeyModifiers::CONTROL, KeyCode::Left, Editor::move_word_left),
    (
        KeyModifiers::ALT,
        KeyCode::Char('f'),
        Editor::move_word_right,
    ),
    (KeyModifiers::ALT, KeyCode::Right, Editor::move_word_right),
    (
        KeyModifiers::CONTROL,
        KeyCode::Right,
        Editor::move_word_right,
    ),
    (KeyModifiers::NONE, KeyCode::Backspace, Editor::delete_left),
    (
        KeyModifiers::CONTROL,
        KeyCode::Char('h'),
        Editor::delete_left,
    ),
    (KeyModifiers::NONE, KeyCode::Delete, Editor::delete_right),
    (
        KeyModifiers::CONTROL,
        KeyCode::Char('d'),
        Editor::delete_right,
    ),
    (
        KeyModifiers::CONTROL,
        KeyCode::Char('u'),
        Editor::kill_line_start,
    ),
    (
        KeyModifiers::CONTROL,
        KeyCode::Char('k'),
        Editor::kill_line_end,
    ),
    (
        KeyModifiers::CONTROL,
        KeyCode::Char('w'),
        Editor::kill_big_word_left,
    ),
    (
        KeyModifiers::ALT,
        KeyCode::Backspace,
        Editor::kill_word_left,
    ),
    (
        KeyModifiers::ALT,
        KeyCode::Char('d'),
        Editor::kill_word_right,
    ),
    (KeyModifiers::CONTROL, KeyCode::Char('y'), Editor::yank),
];

/// Collects input from the person, with shell-style editing keys, and submits it when they
/// press Enter.
///
/// It submits whatever is typed, leaving the App to decide whether that can happen now. Long input
/// wraps onto more rows, so the App should give it the height [`Prompt::height`] asks for.
#[derive(Debug, Default)]
pub struct Prompt {
    /// What has been typed since the last submission, and where the cursor is in it.
    editor: Editor,
}

impl Prompt {
    /// Returns how many rows the prompt needs, border included, to show everything typed when it
    /// is `width` columns wide.
    pub fn height(&self, width: u16) -> u16 {
        let (rows, _) = self.wrapped(width.saturating_sub(BORDER));
        to_u16(rows.len()).saturating_add(BORDER)
    }

    /// Returns what is typed broken into rows that fit `width` columns after the gutter, and the
    /// row and column the cursor is at.
    fn wrapped(&self, width: u16) -> (Vec<String>, (usize, usize)) {
        wrap(self.editor.text(), self.editor.cursor(), width)
    }

    /// Creates a prompt with `draft` already typed.
    pub fn with_draft(draft: &Draft) -> Self {
        Self {
            editor: Editor::with_text(&draft.text, draft.cursor),
        }
    }

    /// Hands over what is typed, leaving the prompt empty.
    pub fn take_draft(&mut self) -> Draft {
        let cursor = self.editor.cursor();
        Draft {
            text: self.editor.take(),
            cursor,
        }
    }

    /// Makes the edit bound to `key`, if there is one.
    fn edit(&mut self, key: KeyEvent) -> KeyOutcome {
        let binding = BINDINGS
            .iter()
            .find(|(modifiers, code, _)| *modifiers == key.modifiers && *code == key.code);
        let Some((_, _, edit)) = binding else {
            return KeyOutcome::Ignored;
        };
        edit(&mut self.editor);
        KeyOutcome::Handled
    }
}

impl Component for Prompt {
    fn update(&mut self, event: &Event) {
        if let Event::Paste(text) = event {
            self.editor.insert_str(&normalize_newlines(text));
        }
    }

    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        let typed = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(character) if typed => self.editor.insert(character),
            KeyCode::Enter => return KeyOutcome::Act(Action::Submit(self.editor.take())),
            _ => return self.edit(key),
        }
        KeyOutcome::Handled
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().border_style(theme::BORDER);
        let inner = block.inner(area);
        let (rows, (row, column)) = self.wrapped(inner.width);
        let top = (row + 1).saturating_sub(usize::from(inner.height));
        let plain = |(index, row)| gutter(index, vec![Span::raw(row)]);
        let lines: Vec<Line> = rows.into_iter().enumerate().map(plain).collect();
        let paragraph = Paragraph::new(lines).scroll((to_u16(top), 0));
        let paragraph = paragraph.style(theme::INPUT);
        frame.render_widget(paragraph.block(block), area);
        let x = inner.x.saturating_add(to_u16(GUTTER + column));
        let y = inner.y.saturating_add(to_u16(row.saturating_sub(top)));
        frame.set_cursor_position((x.min(inner.right()), y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::{ctrl, key, rows, screen, type_into};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Returns a press of `code` with Alt held.
    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    /// Presses each key in turn, checking the prompt used every one.
    fn press(prompt: &mut Prompt, keys: &[KeyEvent]) {
        for pressed in keys {
            assert_eq!(prompt.key(*pressed), KeyOutcome::Handled, "{pressed:?}");
        }
    }

    /// Draws the prompt `width` columns wide at the height it asks for, returning the rows and
    /// where the cursor ends up.
    fn draw(prompt: &Prompt, width: u16) -> (Vec<String>, (u16, u16)) {
        let backend = TestBackend::new(width, prompt.height(width));
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| prompt.render(frame, frame.area()))
            .unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        (rows(&terminal), (cursor.x, cursor.y))
    }

    #[test]
    fn typed_and_pasted_text_is_submitted_on_enter_and_then_cleared() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "helo");
        assert_eq!(prompt.key(key(KeyCode::Backspace)), KeyOutcome::Handled);
        prompt.update(&Event::Paste("lo\r\nthere\rfriend".into()));

        let submitted = prompt.key(key(KeyCode::Enter));

        assert_eq!(
            submitted,
            KeyOutcome::Act(Action::Submit("hello\nthere\nfriend".into()))
        );
        assert_eq!(prompt.editor.text(), "");
    }

    #[test]
    fn shell_keys_edit_where_the_cursor_is() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "git add src/main");
        press(
            &mut prompt,
            &[ctrl('w'), ctrl('a'), alt(KeyCode::Char('f'))],
        );
        type_into(&mut prompt, " -p");
        press(&mut prompt, &[ctrl('e'), ctrl('y'), key(KeyCode::Left)]);
        assert_eq!(prompt.editor.text(), "git -p add src/main");

        press(&mut prompt, &[ctrl('u'), ctrl('k')]);
        assert_eq!(prompt.editor.text(), "");
    }

    #[test]
    fn shortcuts_and_unknown_keys_pass_through() {
        let mut prompt = Prompt::default();
        assert_eq!(prompt.key(ctrl('r')), KeyOutcome::Ignored);
        assert_eq!(prompt.key(alt(KeyCode::Char('z'))), KeyOutcome::Ignored);
        assert_eq!(prompt.key(key(KeyCode::PageUp)), KeyOutcome::Ignored);
        assert_eq!(prompt.editor.text(), "");
    }

    #[test]
    fn what_is_typed_is_shown_after_a_marker() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "hi");
        let (shown, cursor) = draw(&prompt, 10);
        assert_eq!(shown[1], "│> hi    │");
        assert_eq!(cursor, (5, 1));
    }

    #[test]
    fn long_input_wraps_and_the_prompt_grows_to_fit() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "abcdefghij");
        prompt.update(&Event::Paste("\nk".into()));

        let (shown, cursor) = draw(&prompt, 8);

        assert_eq!(
            shown[1..5],
            ["│> abcd│", "│  efgh│", "│  ij  │", "│  k   │"]
        );
        assert_eq!(cursor, (4, 4));
    }

    #[test]
    fn the_cursor_moves_to_a_new_row_once_a_row_is_full() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "abcd");
        let (shown, cursor) = draw(&prompt, 8);
        assert_eq!(shown.len(), 4);
        assert_eq!(cursor, (3, 2));
    }

    #[test]
    fn wide_characters_wrap_whole() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "a日本");
        press(&mut prompt, &[key(KeyCode::Home)]);
        let (shown, cursor) = draw(&prompt, 8);
        // The test buffer shows the second cell of each wide character as a space.
        assert_eq!(shown[1..3], ["│> a日  │", "│  本   │"]);
        assert_eq!(cursor, (3, 1));
    }

    #[test]
    fn a_prompt_too_short_for_its_text_keeps_the_cursor_in_view() {
        let mut prompt = Prompt::default();
        type_into(&mut prompt, "abcdefghij");
        let shown = screen(8, 3, |frame| prompt.render(frame, frame.area()));
        assert_eq!(shown[1], "│  ij  │");
    }
}
