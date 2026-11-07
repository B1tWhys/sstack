//! Where the person types when a real nvim does the editing.

use std::cell::Cell;
use std::ops::Range;

use crossterm::cursor::SetCursorStyle;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use sstack_core::vim::{Draft, Vim, VimEvent, VimMode, VimState};
use unicode_width::UnicodeWidthStr;

use super::wrap::{BORDER, GUTTER, Wrapped, gutter, normalize_newlines, to_u16, wrap_to};
use crate::tui::component::{Component, KeyOutcome};
use crate::tui::event::{Event, SessionSender};
use crate::tui::theme;

/// The keys that have a name in nvim's key notation. `PageUp` and `PageDown` are left out on
/// purpose, so that they keep scrolling the transcript.
const NAMED_KEYS: &[(KeyCode, &str)] = &[
    (KeyCode::Enter, "CR"),
    (KeyCode::Esc, "Esc"),
    (KeyCode::Backspace, "BS"),
    (KeyCode::Delete, "Del"),
    (KeyCode::Insert, "Insert"),
    (KeyCode::Tab, "Tab"),
    (KeyCode::BackTab, "Tab"),
    (KeyCode::Left, "Left"),
    (KeyCode::Right, "Right"),
    (KeyCode::Up, "Up"),
    (KeyCode::Down, "Down"),
    (KeyCode::Home, "Home"),
    (KeyCode::End, "End"),
];

/// Returns `key` in nvim's key notation, or `None` if nvim has no use for it.
fn notation(key: KeyEvent) -> Option<String> {
    let name = key_name(key.code)?;
    let shift =
        key.modifiers.contains(KeyModifiers::SHIFT) && !matches!(key.code, KeyCode::Char(_));
    let prefixes = [
        (key.modifiers.contains(KeyModifiers::CONTROL), "C-"),
        (key.modifiers.contains(KeyModifiers::ALT), "M-"),
        (shift, "S-"),
    ];
    let prefix: String = prefixes
        .into_iter()
        .filter_map(|(held, prefix)| held.then_some(prefix))
        .collect();
    let plain = prefix.is_empty() && name.chars().count() == 1;
    Some(if plain {
        name
    } else {
        format!("<{prefix}{name}>")
    })
}

/// Returns what nvim calls `code`, which is a single character only if it can be sent as it is.
fn key_name(code: KeyCode) -> Option<String> {
    match code {
        KeyCode::Char('<') => Some("lt".to_owned()),
        KeyCode::Char(' ') => Some("Space".to_owned()),
        KeyCode::Char(character) => Some(character.to_string()),
        KeyCode::F(number) => Some(format!("F{number}")),
        code => NAMED_KEYS
            .iter()
            .find(|(named, _)| *named == code)
            .map(|(_, name)| (*name).to_owned()),
    }
}

/// Returns what the mode label says.
fn mode_label(mode: VimMode) -> &'static str {
    match mode {
        VimMode::Normal => " NORMAL ",
        VimMode::Insert => " INSERT ",
        VimMode::Replace => " REPLACE ",
        VimMode::Visual => " VISUAL ",
        VimMode::Command => " COMMAND ",
    }
}

/// Returns `rows`, the wrapped form of `text`, with the characters in `selection` picked out.
fn highlighted(
    rows: Vec<String>,
    text: &str,
    selection: &[Range<usize>],
) -> Vec<Vec<Span<'static>>> {
    let mut offsets = text
        .char_indices()
        .filter(|(_, character)| *character != '\n')
        .map(|(offset, _)| offset);
    let style_at = |offset: usize| {
        let selected = selection.iter().any(|range| range.contains(&offset));
        if selected {
            theme::SELECTION
        } else {
            theme::INPUT
        }
    };
    let mut styled_row = |row: String| {
        row.chars()
            .zip(offsets.by_ref())
            .map(|(character, offset)| Span::styled(character.to_string(), style_at(offset)))
            .collect()
    };
    rows.into_iter().map(&mut styled_row).collect()
}

/// Returns `row`, already styled, with the line number `lead` before it.
fn numbered(lead: String, row: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(lead, theme::MARKER)];
    spans.extend(row);
    Line::from(spans)
}

/// Passes what the person types to a real nvim and draws what comes back, without knowing any
/// vim behavior itself. Alt-Enter submits, because Enter means what it means in vim.
#[derive(Debug)]
pub struct VimPrompt {
    /// Does the editing.
    vim: Vim,
    /// What nvim last reported, or the draft until it has.
    state: VimState,
    /// The size last given to nvim, as rows then columns. Interior mutability because the area is
    /// only known while drawing.
    size: Cell<(u16, u16)>,
}

impl VimPrompt {
    /// Starts nvim editing `draft`, with its reports going to `events`.
    pub fn start(draft: Draft, events: SessionSender) -> Self {
        let state = VimState {
            text: draft.text.clone(),
            cursor: draft.cursor,
            mode: VimMode::Insert,
            selection: Vec::new(),
            line_numbers: Vec::new(),
            cmdline: None,
            message: None,
        };
        let vim = Vim::builder()
            .on_event(move |event| events.send(Event::Vim(event)))
            .draft(draft)
            // Tests must not depend on the config of whoever runs them.
            .clean(cfg!(test))
            .start();
        Self {
            vim,
            state,
            size: Cell::default(),
        }
    }

    /// Returns the text so far, with its cursor.
    pub fn draft(&self) -> Draft {
        Draft::from(&self.state)
    }

    /// Returns how many rows the input needs, border included, to show all the text when it is
    /// `width` columns wide.
    pub fn height(&self, width: u16) -> u16 {
        let wrapped = self.wrapped(width.saturating_sub(BORDER));
        to_u16(wrapped.rows.len()).saturating_add(BORDER)
    }

    /// Returns the cursor shape for the current mode.
    pub fn cursor_style(&self) -> SetCursorStyle {
        match self.state.mode {
            VimMode::Insert => SetCursorStyle::SteadyBar,
            VimMode::Replace => SetCursorStyle::SteadyUnderScore,
            VimMode::Normal | VimMode::Visual | VimMode::Command => SetCursorStyle::SteadyBlock,
        }
    }

    /// Returns how many columns come before the text: the line numbers if nvim has any, or else
    /// the marker.
    fn lead_columns(&self) -> usize {
        self.state
            .line_numbers
            .first()
            .map_or(GUTTER, |label| label.width())
    }

    /// Returns the text laid out to fit `width` columns, what comes before it included.
    fn wrapped(&self, width: u16) -> Wrapped {
        let columns = usize::from(width).saturating_sub(self.lead_columns());
        wrap_to(&self.state.text, self.state.cursor, columns)
    }

    /// Returns what goes before the row at `index` if nvim has line numbers. Only a line's first
    /// row gets a number, as in nvim.
    fn number_lead(&self, index: usize, line_starts: &[usize]) -> Option<String> {
        let numbers = &self.state.line_numbers;
        if numbers.is_empty() {
            return None;
        }
        let line = line_starts.binary_search(&index).ok();
        let label = line.and_then(|line| numbers.get(line)).cloned();
        Some(label.unwrap_or_else(|| " ".repeat(self.lead_columns())))
    }

    /// Tells nvim the size of `area`, the text, if that changed. Line numbers are left in, because
    /// nvim takes them out itself.
    fn fit_to(&self, area: Rect) {
        let marker = if self.state.line_numbers.is_empty() {
            GUTTER
        } else {
            0
        };
        let columns = area.width.saturating_sub(to_u16(marker));
        let size = (area.height, columns);
        if self.size.replace(size) != size {
            self.vim.resize(area.height, columns);
        }
    }

    /// Draws the text into `area`, returning where the cursor belongs in it.
    fn render_text(&self, frame: &mut Frame, area: Rect) -> (u16, u16) {
        let wrapped = self.wrapped(area.width);
        let (row, column) = wrapped.cursor;
        let top = (row + 1).saturating_sub(usize::from(area.height));
        let styled = highlighted(wrapped.rows, &self.state.text, &self.state.selection);
        let lines: Vec<Line> = styled
            .into_iter()
            .enumerate()
            .map(
                |(index, row)| match self.number_lead(index, &wrapped.line_starts) {
                    Some(lead) => numbered(lead, row),
                    None => gutter(index, row),
                },
            )
            .collect();
        let paragraph = Paragraph::new(lines).scroll((to_u16(top), 0));
        frame.render_widget(paragraph.style(theme::INPUT), area);
        let x = area.x.saturating_add(to_u16(self.lead_columns() + column));
        let y = area.y.saturating_add(to_u16(row.saturating_sub(top)));
        (x.min(area.right().saturating_sub(1)), y)
    }

    /// Draws the command line or latest error over the bottom border in `area`, so that pressing
    /// `:` does not move everything else on screen, returning where the cursor belongs if it is
    /// on the command line.
    fn render_cmdline(&self, frame: &mut Frame, area: Rect) -> Option<(u16, u16)> {
        if let Some(cmdline) = &self.state.cmdline {
            frame.render_widget(Paragraph::new(cmdline.text.as_str()), area);
            let typed = cmdline.text.get(..cmdline.cursor).unwrap_or_default();
            return Some((area.x.saturating_add(to_u16(typed.width())), area.y));
        }
        if let Some(message) = &self.state.message {
            let paragraph = Paragraph::new(message.as_str()).style(theme::ATTENTION);
            frame.render_widget(paragraph, area);
        }
        None
    }
}

impl Component for VimPrompt {
    fn update(&mut self, event: &Event) {
        match event {
            Event::Vim(VimEvent::State(state)) => self.state = state.clone(),
            // Nvim empties its buffer next, but anything reading the draft cannot wait for that.
            Event::Vim(VimEvent::Submitted(_)) => {
                self.state.text.clear();
                self.state.cursor = 0;
            }
            Event::Paste(text) => self.vim.paste(&normalize_newlines(text)),
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::ALT {
            self.vim.submit();
            return KeyOutcome::Handled;
        }
        let Some(keys) = notation(key) else {
            return KeyOutcome::Ignored;
        };
        self.vim.send_keys(&keys);
        KeyOutcome::Handled
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let label = Line::styled(mode_label(self.state.mode), theme::MODE);
        let block = Block::bordered()
            .border_style(theme::BORDER)
            .title(label.alignment(Alignment::Right));
        let text = block.inner(area);
        frame.render_widget(block, area);
        self.fit_to(text);
        let in_text = self.render_text(frame, text);
        let bottom_border = Rect {
            y: text.bottom(),
            height: 1,
            ..text
        };
        let on_cmdline = self.render_cmdline(frame, bottom_border.intersection(area));
        frame.set_cursor_position(on_cmdline.unwrap_or(in_text));
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use sstack_core::vim::{VimCmdline, VimFailure};
    use tokio::sync::mpsc;

    use super::*;
    use crate::tui::event::EventReceiver;
    use crate::tui::test_support::{alt, ctrl, key, rows};

    /// Returns a press of `code` with `modifiers` held.
    fn with(modifiers: KeyModifiers, code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// Returns a prompt that was never given a chance to start nvim, which is all that drawing
    /// needs, because the test's single thread stays busy.
    fn idle_prompt() -> (VimPrompt, EventReceiver) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let prompt = VimPrompt::start(Draft::default(), SessionSender::new("test", sender));
        (prompt, receiver)
    }

    /// Returns a state showing `text` in `mode`.
    fn state(text: &str, mode: VimMode) -> VimState {
        VimState {
            text: text.to_owned(),
            cursor: text.len(),
            mode,
            selection: Vec::new(),
            line_numbers: Vec::new(),
            cmdline: None,
            message: None,
        }
    }

    /// Draws `prompt` in `width` by `height`, returning the screen and cursor.
    fn draw(prompt: &VimPrompt, width: u16, height: u16) -> (Vec<String>, (u16, u16)) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| prompt.render(frame, frame.area()))
            .unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        (rows(&terminal), (cursor.x, cursor.y))
    }

    #[test]
    fn keys_become_nvim_notation() {
        let cases = [
            (key(KeyCode::Char('a')), "a"),
            (with(KeyModifiers::SHIFT, KeyCode::Char('A')), "A"),
            (key(KeyCode::Char('<')), "<lt>"),
            (key(KeyCode::Char(' ')), "<Space>"),
            (ctrl('a'), "<C-a>"),
            (alt('j'), "<M-j>"),
            (with(KeyModifiers::CONTROL, KeyCode::Char('<')), "<C-lt>"),
            (key(KeyCode::Enter), "<CR>"),
            (key(KeyCode::Esc), "<Esc>"),
            (key(KeyCode::F(5)), "<F5>"),
            (with(KeyModifiers::SHIFT, KeyCode::BackTab), "<S-Tab>"),
            (with(KeyModifiers::SHIFT, KeyCode::Left), "<S-Left>"),
        ];
        for (pressed, expected) in cases {
            assert_eq!(notation(pressed).as_deref(), Some(expected), "{pressed:?}");
        }
    }

    #[test]
    fn keys_for_the_transcript_are_not_nvims() {
        assert_eq!(notation(key(KeyCode::PageUp)), None);
        assert_eq!(notation(key(KeyCode::PageDown)), None);
    }

    #[test]
    fn every_mode_has_a_label_and_cursor() {
        assert_eq!(mode_label(VimMode::Normal), " NORMAL ");
        assert_eq!(mode_label(VimMode::Insert), " INSERT ");
        assert_eq!(mode_label(VimMode::Replace), " REPLACE ");
        assert_eq!(mode_label(VimMode::Visual), " VISUAL ");
        assert_eq!(mode_label(VimMode::Command), " COMMAND ");
    }

    #[tokio::test]
    async fn the_cursor_shape_follows_the_mode() {
        let (mut prompt, _events) = idle_prompt();
        assert_eq!(prompt.cursor_style(), SetCursorStyle::SteadyBar);
        for (mode, shape) in [
            (VimMode::Normal, SetCursorStyle::SteadyBlock),
            (VimMode::Replace, SetCursorStyle::SteadyUnderScore),
        ] {
            prompt.update(&Event::Vim(VimEvent::State(state("", mode))));
            assert_eq!(prompt.cursor_style(), shape);
        }
    }

    #[tokio::test]
    async fn the_mode_shows_in_the_border_and_the_text_inside_it() {
        let (mut prompt, _events) = idle_prompt();
        let shown = state("hello", VimMode::Normal);
        prompt.update(&Event::Vim(VimEvent::State(shown)));
        let (screen, cursor) = draw(&prompt, 30, 4);
        assert!(screen[0].contains("NORMAL"), "{screen:?}");
        assert_eq!(screen[1], "\u{2502}> hello                     \u{2502}");
        assert_eq!(cursor, (8, 1));
    }

    #[tokio::test]
    async fn empty_input_needs_room_for_one_line() {
        let (prompt, _events) = idle_prompt();
        assert_eq!(prompt.height(30), 3);
        let (screen, cursor) = draw(&prompt, 30, 3);
        assert!(screen[0].starts_with('\u{250c}'), "{screen:?}");
        assert_eq!(screen[1], "\u{2502}>                           \u{2502}");
        assert!(screen[2].starts_with('\u{2514}'), "{screen:?}");
        assert_eq!(cursor, (3, 1));
    }

    #[tokio::test]
    async fn long_text_scrolls_to_keep_the_cursor_in_view() {
        let (mut prompt, _events) = idle_prompt();
        prompt.update(&Event::Vim(VimEvent::State(state(
            "one\ntwo\nthree",
            VimMode::Insert,
        ))));
        assert_eq!(prompt.height(30), 5);
        let (screen, cursor) = draw(&prompt, 30, 4);
        assert_eq!(screen[2], "\u{2502}  three                     \u{2502}");
        assert_eq!(cursor, (8, 2));
    }

    #[tokio::test]
    async fn line_numbers_replace_the_marker_on_each_lines_first_row() {
        let (mut prompt, _events) = idle_prompt();
        let mut shown = state("abcdefghij\nk", VimMode::Normal);
        shown.line_numbers = vec!["1   ".to_owned(), "  1 ".to_owned()];
        prompt.update(&Event::Vim(VimEvent::State(shown)));
        assert_eq!(prompt.height(10), 6);
        let (screen, cursor) = draw(&prompt, 10, 6);
        assert_eq!(screen[1], "\u{2502}1   abcd\u{2502}");
        assert_eq!(screen[2], "\u{2502}    efgh\u{2502}");
        assert_eq!(screen[3], "\u{2502}    ij  \u{2502}");
        assert_eq!(screen[4], "\u{2502}  1 k   \u{2502}");
        assert_eq!(cursor, (6, 4));
    }

    #[tokio::test]
    async fn the_selection_is_picked_out_and_nothing_else() {
        let (mut prompt, _events) = idle_prompt();
        let mut shown = state("abc\ndef", VimMode::Visual);
        shown.selection = vec![1..2, 4..5];
        prompt.update(&Event::Vim(VimEvent::State(shown)));
        let mut terminal = Terminal::new(TestBackend::new(30, 5)).unwrap();
        terminal
            .draw(|frame| prompt.render(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let is_selected = |x, y| buffer[(x, y)].bg == theme::SELECTION.bg.unwrap();
        assert!(is_selected(4, 1));
        assert!(is_selected(3, 2));
        assert!(!is_selected(3, 1));
        assert!(!is_selected(5, 1));
    }

    #[tokio::test]
    async fn the_command_line_takes_the_cursor_with_it() {
        let (mut prompt, _events) = idle_prompt();
        let mut shown = state("text", VimMode::Command);
        shown.cmdline = Some(VimCmdline {
            text: ":wq".to_owned(),
            cursor: 2,
        });
        prompt.update(&Event::Vim(VimEvent::State(shown)));
        let (screen, cursor) = draw(&prompt, 30, 3);
        assert_eq!(screen[1], "\u{2502}> text                      \u{2502}");
        assert_eq!(
            screen[2],
            format!("\u{2514}:wq{}\u{2518}", "\u{2500}".repeat(25))
        );
        assert_eq!(cursor, (3, 2));
    }

    #[tokio::test]
    async fn an_error_shows_on_the_bottom_border() {
        let (mut prompt, _events) = idle_prompt();
        let mut shown = state("text", VimMode::Normal);
        shown.message = Some("E492: nope".to_owned());
        prompt.update(&Event::Vim(VimEvent::State(shown)));
        let (screen, cursor) = draw(&prompt, 30, 3);
        assert_eq!(
            screen[2],
            format!("\u{2514}E492: nope{}\u{2518}", "\u{2500}".repeat(18))
        );
        assert_eq!(cursor, (7, 1));
    }

    #[tokio::test]
    async fn submitting_empties_the_draft_at_once() {
        let (mut prompt, _events) = idle_prompt();
        let shown = state("hello", VimMode::Insert);
        prompt.update(&Event::Vim(VimEvent::State(shown)));
        assert_eq!(prompt.draft().text, "hello");
        prompt.update(&Event::Vim(VimEvent::Submitted("hello".to_owned())));
        assert_eq!(prompt.draft(), Draft::default());
    }

    #[tokio::test]
    async fn reports_meant_for_others_change_nothing() {
        let (mut prompt, _events) = idle_prompt();
        prompt.update(&Event::Resize);
        prompt.update(&Event::Vim(VimEvent::Lost {
            draft: Draft::default(),
            failure: VimFailure::TimedOut,
        }));
        assert_eq!(prompt.state.mode, VimMode::Insert);
    }

    /// Drives a real nvim through the prompt.
    #[cfg(feature = "nvim-tests")]
    mod real {
        use std::time::Duration;

        use tokio::time::timeout;

        use super::*;
        use crate::tui::test_support::type_into;

        /// The longest a test waits for nvim before failing.
        const PATIENCE: Duration = Duration::from_secs(10);

        /// Passes what nvim says to `prompt` until one of its reports passes `check`.
        async fn until(
            prompt: &mut VimPrompt,
            events: &mut EventReceiver,
            check: impl Fn(&Event) -> bool,
        ) {
            loop {
                let event = timeout(PATIENCE, events.recv()).await.unwrap().unwrap();
                let Event::Session(_, event) = event else {
                    continue;
                };
                prompt.update(&event);
                if check(&event) {
                    return;
                }
            }
        }

        /// Waits until `prompt` shows `text`.
        async fn until_text(prompt: &mut VimPrompt, events: &mut EventReceiver, text: &str) {
            let shows_text = |event: &Event| matches!(event, Event::Vim(VimEvent::State(state)) if state.text == text);
            until(prompt, events, shows_text).await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn typing_edits_with_real_vim_and_alt_enter_submits() {
            let (mut prompt, mut events) = idle_prompt();
            type_into(&mut prompt, "one two");
            until_text(&mut prompt, &mut events, "one two").await;
            assert_eq!(prompt.key(key(KeyCode::Esc)), KeyOutcome::Handled);
            type_into(&mut prompt, "bdw");
            until_text(&mut prompt, &mut events, "one ").await;
            prompt.key(with(KeyModifiers::ALT, KeyCode::Enter));
            let submitted = |event: &Event| matches!(event, Event::Vim(VimEvent::Submitted(text)) if text == "one ");
            until(&mut prompt, &mut events, submitted).await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn pasting_goes_to_nvim_with_clean_newlines() {
            let (mut prompt, mut events) = idle_prompt();
            prompt.update(&Event::Paste("one\r\ntwo".to_owned()));
            until_text(&mut prompt, &mut events, "one\ntwo").await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn wrapping_matches_nvim_with_line_numbers_on() {
            let (mut prompt, mut events) = idle_prompt();
            prompt.key(key(KeyCode::Esc));
            type_into(&mut prompt, ":set number");
            prompt.key(key(KeyCode::Enter));
            let numbered = |event: &Event| matches!(event, Event::Vim(VimEvent::State(state)) if !state.line_numbers.is_empty());
            until(&mut prompt, &mut events, numbered).await;
            draw(&prompt, 26, 5);
            let long = "x".repeat(30);
            prompt.update(&Event::Paste(long.clone()));
            until_text(&mut prompt, &mut events, &long).await;
            prompt.key(key(KeyCode::Esc));
            type_into(&mut prompt, "0gjx");
            until_text(&mut prompt, &mut events, &"x".repeat(29)).await;
            let (screen, _) = draw(&prompt, 26, 5);
            assert_eq!(screen[1], format!("\u{2502}  1 {}\u{2502}", "x".repeat(20)));
            assert_eq!(prompt.state.cursor, 20);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn drawing_tells_nvim_the_size_of_the_text() {
            let (mut prompt, mut events) = idle_prompt();
            draw(&prompt, 22, 5);
            let long = "x".repeat(30);
            prompt.update(&Event::Paste(long.clone()));
            until_text(&mut prompt, &mut events, &long).await;
            prompt.key(key(KeyCode::Esc));
            type_into(&mut prompt, "0gjx");
            until_text(&mut prompt, &mut events, &"x".repeat(29)).await;
            assert_eq!(prompt.state.cursor, 18);
        }
    }
}
