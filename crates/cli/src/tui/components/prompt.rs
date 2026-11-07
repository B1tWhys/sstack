//! Where the person types.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::tui::component::{Component, KeyOutcome};
use crate::tui::editor::{Edit, Editor};
use crate::tui::event::{Action, Event};
use crate::tui::theme;

/// Shown before the first row of what the person types.
const MARKER: &str = "> ";

/// Shown before every later row, so wrapped text lines up after the marker.
const INDENT: &str = "  ";

/// How many columns the marker and indent take.
const GUTTER: usize = MARKER.len();

/// How the border around the prompt adds to its height and width.
const BORDER: u16 = 2;

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
        let (before, after) = self.editor.text().split_at(self.editor.cursor());
        let mut rows = Rows::new(usize::from(width).saturating_sub(GUTTER));
        before.chars().for_each(|character| rows.push(character));
        let cursor = rows.position();
        after.chars().for_each(|character| rows.push(character));
        (rows.finish(), cursor)
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
        let lines: Vec<Line> = rows.into_iter().enumerate().map(gutter).collect();
        let paragraph = Paragraph::new(lines).scroll((to_u16(top), 0));
        let paragraph = paragraph.style(theme::INPUT);
        frame.render_widget(paragraph.block(block), area);
        let x = inner.x.saturating_add(to_u16(GUTTER + column));
        let y = inner.y.saturating_add(to_u16(row.saturating_sub(top)));
        frame.set_cursor_position((x.min(inner.right()), y));
    }
}

/// Returns the row at `index` with the marker or indent before it.
fn gutter((index, row): (usize, String)) -> Line<'static> {
    let lead = if index == 0 { MARKER } else { INDENT };
    Line::from(vec![Span::styled(lead, theme::MARKER), Span::raw(row)])
}

/// Returns `text` with every line break as a plain newline, since terminals paste them as
/// carriage returns, which would garble the screen.
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Converts a count to screen units, saturating rather than wrapping when it is too big.
fn to_u16(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// Lays out text into rows of a fixed width, one character at a time, breaking at newlines and
/// wherever the next character would not fit.
struct Rows {
    /// The rows before the one being filled.
    full: Vec<String>,
    /// The row being filled.
    row: String,
    /// How many columns a row can hold. Always at least one.
    width: usize,
    /// How many columns of the row being filled are used.
    column: usize,
}

impl Rows {
    /// Starts laying out rows `width` columns wide.
    fn new(width: usize) -> Self {
        Self {
            full: Vec::new(),
            row: String::new(),
            width: width.max(1),
            column: 0,
        }
    }

    /// Adds `character`, starting a new row first if it would not fit.
    fn push(&mut self, character: char) {
        if character == '\n' {
            self.break_row();
            return;
        }
        let width = character.width().unwrap_or(0);
        if self.column + width > self.width {
            self.break_row();
        }
        self.row.push(character);
        self.column += width;
    }

    /// Returns the row and column where the next character would go, starting a new row if the
    /// current one is full.
    fn position(&mut self) -> (usize, usize) {
        if self.column >= self.width {
            self.break_row();
        }
        (self.full.len(), self.column)
    }

    /// Starts a new, empty row.
    fn break_row(&mut self) {
        self.full.push(std::mem::take(&mut self.row));
        self.column = 0;
    }

    /// Returns every row, ending with the one being filled even if it is empty.
    fn finish(mut self) -> Vec<String> {
        self.full.push(self.row);
        self.full
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
