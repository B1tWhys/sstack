//! Where the person types when a real nvim does the editing, drawn on a grid of monospace cells
//! so that it wraps at the same columns nvim moves the cursor by.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::advanced::text::{Paragraph as _, Text};
use iced::keyboard::{self, Key, Modifiers, key::Named};
use iced::widget::text::{Alignment, LineHeight, Shaping, Span, Wrapping};
use iced::widget::{column, container, pin, rich_text, row, sensor, space, span, stack, text};
use iced::{Color, Element, Fill, Font, Pixels, Point, Size, Subscription, Theme, alignment};
use sstack::vim::layout::{Wrapped, wrap_to};
use sstack::vim::{Draft, Vim, VimEvent, VimMode, VimState};

use crate::inbox::Inbox;
use crate::window::Message;

/// The size of the prompt's text, in logical pixels.
const TEXT_SIZE: f32 = 16.0;

/// The marker shown before the first row of what the person types, when nvim shows no line
/// numbers.
const MARKER: &str = "> ";

/// The indent shown before every later row, so wrapped text lines up after the marker.
const INDENT: &str = "  ";

/// How many cells the marker and indent take.
const GUTTER: usize = MARKER.len();

/// Space between the edge of the prompt's box and the text, in logical pixels.
const PADDING: f32 = 6.0;

/// How round the corners of the prompt's box are, in logical pixels.
const CORNER_RADIUS: f32 = 4.0;

/// How thick the Insert mode bar and the Replace mode underline are, in logical pixels.
const CURSOR_LINE: f32 = 2.0;

/// The color of the cursor and of selected text, which reads on light and dark backgrounds alike.
const HIGHLIGHT: Color = Color::from_rgb(0.3, 0.5, 1.0);

/// How opaque the block cursor is, so that the character under it stays readable.
const BLOCK_ALPHA: f32 = 0.5;

/// How opaque the background of selected text is.
const SELECTION_ALPHA: f32 = 0.3;

/// How many characters are measured to find the width of one cell, which is more precise than
/// measuring one.
const MEASURED: usize = 100;

/// The [`VimPrompt::key`] the next prompt started gets.
static NEXT_KEY: AtomicU64 = AtomicU64::new(0);

/// The keys that have a name in nvim's key notation. `PageUp` and `PageDown` are left out, as in
/// the TUI, where they scroll the transcript.
const NAMED_KEYS: &[(Named, &str)] = &[
    (Named::Enter, "CR"),
    (Named::Escape, "Esc"),
    (Named::Backspace, "BS"),
    (Named::Delete, "Del"),
    (Named::Insert, "Insert"),
    (Named::Tab, "Tab"),
    (Named::Space, "Space"),
    (Named::ArrowLeft, "Left"),
    (Named::ArrowRight, "Right"),
    (Named::ArrowUp, "Up"),
    (Named::ArrowDown, "Down"),
    (Named::Home, "Home"),
    (Named::End, "End"),
    (Named::F1, "F1"),
    (Named::F2, "F2"),
    (Named::F3, "F3"),
    (Named::F4, "F4"),
    (Named::F5, "F5"),
    (Named::F6, "F6"),
    (Named::F7, "F7"),
    (Named::F8, "F8"),
    (Named::F9, "F9"),
    (Named::F10, "F10"),
    (Named::F11, "F11"),
    (Named::F12, "F12"),
];

/// What the window still has to do about a key the prompt was offered.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Nothing: the prompt used the key.
    Handled,
    /// Read the clipboard and hand what is on it to [`VimPrompt::paste`], because the person
    /// asked to paste.
    Paste,
    /// Nothing: the key means nothing to nvim.
    Ignored,
}

/// A stretch of one row on screen that is either all selected or all not.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    /// The characters.
    text: String,
    /// Whether they are selected.
    selected: bool,
}

/// The prompt that passes what the person types to a real nvim and draws what comes back,
/// without knowing any vim behavior itself. Enter submits in Normal mode, where nvim decides what
/// Enter does so that it applies in the mode nvim is really in. The window's own send key submits
/// in any mode.
pub struct VimPrompt {
    /// The nvim that does the editing.
    vim: Vim,
    /// What nvim reports, on its way to the window.
    events: Inbox<VimEvent>,
    /// What nvim last reported, or the draft until it has.
    state: VimState,
    /// The size of one cell of the grid, in logical pixels.
    cell: Size,
    /// How wide the prompt is, in logical pixels, once the window has laid it out.
    width: Option<f32>,
    /// The size last given to nvim, as rows then columns.
    size: (u16, u16),
    /// What sets this prompt apart from every other, so that iced measures it afresh when it
    /// takes the place of another, as a new Session's prompt does, instead of treating it as one
    /// already measured.
    key: u64,
}

impl VimPrompt {
    /// Starts nvim editing `draft`. What it reports arrives through [`Self::subscription`].
    ///
    /// Must be called inside a Tokio runtime, which runs nvim's side of the conversation.
    pub fn start(draft: Draft) -> Self {
        let state = VimState {
            text: draft.text.clone(),
            cursor: draft.cursor,
            mode: VimMode::Insert,
            selection: Vec::new(),
            line_numbers: Vec::new(),
            cmdline: None,
            message: None,
        };
        let (events, sender) = Inbox::new();
        let vim = Vim::builder()
            .on_event(move |event| {
                let _ = sender.unbounded_send(event);
            })
            .draft(draft)
            // Tests must not depend on the config of whoever runs them.
            .clean(cfg!(test))
            .start();
        Self {
            vim,
            events,
            state,
            cell: cell_size(),
            width: None,
            size: (0, 0),
            key: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Returns the text so far, with its cursor.
    pub fn draft(&self) -> Draft {
        Draft::from(&self.state)
    }

    /// Returns where what nvim reports comes from. It ends when this prompt is dropped.
    pub fn subscription(&self) -> Subscription<VimEvent> {
        self.events.subscription()
    }

    /// Returns what nvim reports as a stream, which can be taken once.
    #[cfg(test)]
    pub fn events(&self) -> impl iced::futures::Stream<Item = VimEvent> + use<> {
        self.events.stream()
    }

    /// Shows what nvim reported. Losing nvim is for the window to act on.
    pub fn update(&mut self, event: &VimEvent) {
        match event {
            VimEvent::State(state) => self.state = state.clone(),
            // Nvim empties its buffer next, but anything reading the draft cannot wait for that.
            VimEvent::Submitted(_) => {
                self.state.text.clear();
                self.state.cursor = 0;
            }
            VimEvent::Lost { .. } => {}
        }
        self.fit();
    }

    /// Passes a key the person pressed on to nvim, returning what the window still has to do
    /// about it. Command-V asks to paste, because the Command keys belong to the system.
    pub fn key(&self, event: &keyboard::Event) -> KeyOutcome {
        let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
            return KeyOutcome::Ignored;
        };
        if modifiers.logo() && key.as_ref() == Key::Character("v") {
            return KeyOutcome::Paste;
        }
        notation(event).map_or(KeyOutcome::Ignored, |keys| {
            self.vim.send_keys(&keys);
            KeyOutcome::Handled
        })
    }

    /// Inserts `text` in one piece, whichever mode nvim is in.
    pub fn paste(&self, text: &str) {
        self.vim
            .paste(&text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    /// Asks nvim for the text, which comes back as [`VimEvent::Submitted`].
    pub fn submit(&self) {
        self.vim.submit();
    }

    /// Notes that the window laid the prompt out at `size`, in logical pixels.
    pub fn resized(&mut self, size: Size) {
        self.width = Some(size.width);
        self.fit();
    }

    /// Draws the prompt: the text, with the cursor and any selection, above the command line or
    /// latest error and the mode.
    pub fn view(&self) -> Element<'_, Message> {
        let wrapped = self.wrapped();
        let runs = runs(&wrapped.rows, &self.state.text, &self.state.selection);
        let rows = runs.into_iter().enumerate().map(|(index, runs)| {
            let lead = self.lead(index, &wrapped.line_starts);
            grid_row(lead, runs)
        });
        let text_area = column(rows).width(Fill);
        let grid = stack![text_area].push(self.cursor(wrapped.cursor));
        let mode = text(mode_label(self.state.mode)).font(Font::MONOSPACE);
        let status = row![self.status_line(), space::horizontal(), mode];
        // The sensor sits inside the padding, so that only the room for text counts as width.
        let measured = sensor(column![grid, status].width(Fill))
            .key(self.key)
            .on_show(Message::PromptResized)
            .on_resize(Message::PromptResized);
        container(measured)
            .width(Fill)
            .padding(PADDING)
            .style(boxed)
            .into()
    }

    /// Returns how many cells fit across the prompt, or `None` until the window has laid it out.
    fn cells_across(&self) -> Option<usize> {
        let width = self.width?;
        // Never negative, and a prompt wider than `usize` cells is not a real window.
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some((width / self.cell.width).floor().max(0.0) as usize)
    }

    /// Returns how many cells come before the text: the line numbers if nvim has any, or else the
    /// marker.
    fn lead_columns(&self) -> usize {
        self.state
            .line_numbers
            .first()
            .map_or(GUTTER, |label| label.chars().count())
    }

    /// Returns the text laid out in rows that fit beside what comes before it. It does not wrap
    /// until the window has laid the prompt out.
    fn wrapped(&self) -> Wrapped {
        let across = self.cells_across().unwrap_or(usize::MAX);
        let columns = across.saturating_sub(self.lead_columns());
        wrap_to(&self.state.text, self.state.cursor, columns)
    }

    /// Tells nvim how big the prompt is, if that changed. Line numbers are left in, because nvim
    /// takes them out itself.
    fn fit(&mut self) {
        let Some(across) = self.cells_across() else {
            return;
        };
        let marker = if self.state.line_numbers.is_empty() {
            GUTTER
        } else {
            0
        };
        let columns = to_u16(across.saturating_sub(marker));
        let rows = to_u16(self.wrapped().rows.len());
        if self.size != (rows, columns) {
            self.size = (rows, columns);
            self.vim.resize(rows, columns);
        }
    }

    /// Returns what goes before the row at `index`: the line number if nvim has them, which only
    /// a line's first row gets, as in nvim, or else the marker or indent.
    fn lead(&self, index: usize, line_starts: &[usize]) -> String {
        let numbers = &self.state.line_numbers;
        if numbers.is_empty() {
            let lead = if index == 0 { MARKER } else { INDENT };
            return lead.to_owned();
        }
        let line = line_starts.binary_search(&index).ok();
        let label = line.and_then(|line| numbers.get(line)).cloned();
        label.unwrap_or_else(|| " ".repeat(self.lead_columns()))
    }

    /// Returns the cursor, placed over the cell at `cursor`, the row then column in the text, and
    /// shaped for the mode. On the command line, the cursor is drawn there instead.
    fn cursor(&self, (row, column): (usize, usize)) -> Option<Element<'_, Message>> {
        if self.state.cmdline.is_some() {
            return None;
        }
        let cell = self.cell;
        let top_left = Point::new(
            cells(self.lead_columns() + column) * cell.width,
            cells(row) * cell.height,
        );
        let (size, offset, alpha) = match self.state.mode {
            VimMode::Insert => (Size::new(CURSOR_LINE, cell.height), 0.0, 1.0),
            VimMode::Replace => (
                Size::new(cell.width, CURSOR_LINE),
                cell.height - CURSOR_LINE,
                1.0,
            ),
            VimMode::Normal | VimMode::Visual | VimMode::Command => (cell, 0.0, BLOCK_ALPHA),
        };
        let shape = container(space())
            .width(size.width)
            .height(size.height)
            .style(move |_: &Theme| shaded(alpha));
        Some(
            pin(shape)
                .position(top_left + iced::Vector::new(0.0, offset))
                .into(),
        )
    }

    /// Returns the command line being typed, with its cursor, or else the latest error, or else
    /// nothing.
    fn status_line(&self) -> Element<'_, Message> {
        if let Some(cmdline) = &self.state.cmdline {
            return rich_text(cmdline_spans(&cmdline.text, cmdline.cursor))
                .font(Font::MONOSPACE)
                .into();
        }
        let message = self.state.message.as_deref().unwrap_or_default();
        text(message)
            .font(Font::MONOSPACE)
            .style(text::danger)
            .into()
    }
}

/// Returns `count` cells as a number of them that can be multiplied by a cell's size.
fn cells(count: usize) -> f32 {
    // A prompt is never anywhere near the size where this loses precision.
    #[expect(clippy::cast_precision_loss)]
    let cells = count as f32;
    cells
}

/// Converts a count to grid units, saturating rather than wrapping when it is too big.
fn to_u16(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// Returns the style of the box around the prompt, a shade off the window's background so that
/// the prompt stands apart from the transcript.
fn boxed(theme: &Theme) -> container::Style {
    let shade = theme.extended_palette().background.weaker;
    container::Style::default()
        .background(shade.color)
        .color(shade.text)
        .border(iced::Border::default().rounded(CORNER_RADIUS))
}

/// Returns the style of something in the highlight color at `alpha` opacity, such as the cursor.
fn shaded(alpha: f32) -> container::Style {
    container::Style::default().background(HIGHLIGHT.scale_alpha(alpha))
}

/// Returns one row of the grid: `lead`, then the row's text with any selection shown.
fn grid_row<'a>(lead: String, runs: Vec<Run>) -> Element<'a, Message> {
    let spans: Vec<Span<'static, ()>> = std::iter::once(span(lead))
        .chain(runs.into_iter().map(run_span))
        .collect();
    rich_text(spans)
        .font(Font::MONOSPACE)
        .size(TEXT_SIZE)
        .wrapping(Wrapping::None)
        .into()
}

/// Returns `run` as text to draw, on a background if it is selected.
fn run_span(run: Run) -> Span<'static, ()> {
    let selected = span(run.text);
    if run.selected {
        selected.background(HIGHLIGHT.scale_alpha(SELECTION_ALPHA))
    } else {
        selected
    }
}

/// Returns the command line `typed`, with the character at the byte offset `cursor` picked out
/// as the cursor, or a space after the end if the cursor is there.
fn cmdline_spans(typed: &str, cursor: usize) -> Vec<Span<'_, ()>> {
    let (before, after) = typed.split_at(cursor.min(typed.len()));
    let mut rest = after.chars();
    let under = rest.next().map_or_else(|| " ".to_owned(), String::from);
    let cursor = span(under).background(HIGHLIGHT.scale_alpha(BLOCK_ALPHA));
    vec![span(before), cursor, span(rest.as_str())]
}

/// Returns `rows`, the wrapped form of `text`, split into runs of characters that are all inside
/// `selection` or all outside it.
fn runs(rows: &[String], text: &str, selection: &[Range<usize>]) -> Vec<Vec<Run>> {
    let mut offsets = text
        .char_indices()
        .filter(|(_, character)| *character != '\n')
        .map(|(offset, _)| offset);
    let selected = |offset: usize| selection.iter().any(|range| range.contains(&offset));
    rows.iter()
        .map(|row| {
            let mut runs = Vec::new();
            for (character, offset) in row.chars().zip(offsets.by_ref()) {
                extend(&mut runs, character, selected(offset));
            }
            runs
        })
        .collect()
}

/// Adds `character` to the end of `runs`, in a new run if it differs in being `selected` from the
/// last.
fn extend(runs: &mut Vec<Run>, character: char, selected: bool) {
    match runs.last_mut() {
        Some(run) if run.selected == selected => run.text.push(character),
        _ => runs.push(Run {
            text: character.to_string(),
            selected,
        }),
    }
}

/// Returns the size of one cell of the grid the prompt is drawn on, in logical pixels.
fn cell_size() -> Size {
    let content = "M".repeat(MEASURED);
    let measured = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph::with_text(Text {
        content: content.as_str(),
        bounds: Size::INFINITE,
        size: Pixels(TEXT_SIZE),
        line_height: LineHeight::default(),
        font: Font::MONOSPACE,
        align_x: Alignment::Default,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Basic,
        wrapping: Wrapping::None,
    })
    .min_bounds();
    Size::new(measured.width / cells(MEASURED), measured.height)
}

/// Returns what the mode label says.
fn mode_label(mode: VimMode) -> &'static str {
    match mode {
        VimMode::Normal => "NORMAL",
        VimMode::Insert => "INSERT",
        VimMode::Replace => "REPLACE",
        VimMode::Visual => "VISUAL",
        VimMode::Command => "COMMAND",
    }
}

/// Returns the key pressed in `event` in nvim's key notation, or `None` if nvim has no use for
/// it. Keys pressed with Command are left to the system.
fn notation(event: &keyboard::Event) -> Option<String> {
    let keyboard::Event::KeyPressed {
        key,
        modified_key,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    if modifiers.logo() {
        return None;
    }
    // Option changes the character typed on macOS, so with Alt the key itself counts.
    let pressed = if modifiers.alt() { key } else { modified_key };
    let (name, named) = match pressed.as_ref() {
        Key::Named(named) => (named_key(named)?.to_owned(), true),
        Key::Character(character) => (character_name(character, *modifiers), false),
        Key::Unidentified => return None,
    };
    Some(with_modifiers(&name, *modifiers, named))
}

/// Returns `name` with the prefixes for the modifiers held, in angle brackets unless it is a
/// single character typed as it is. Shift counts only for a `named` key, since it has already
/// changed any character typed.
fn with_modifiers(name: &str, modifiers: Modifiers, named: bool) -> String {
    let prefixes = [
        (modifiers.control(), "C-"),
        (modifiers.alt(), "M-"),
        (modifiers.shift() && named, "S-"),
    ];
    let prefix: String = prefixes
        .into_iter()
        .filter_map(|(held, prefix)| held.then_some(prefix))
        .collect();
    if prefix.is_empty() && name.chars().count() == 1 {
        name.to_owned()
    } else {
        format!("<{prefix}{name}>")
    }
}

/// Returns what nvim calls a key named `named`, if it has a use for it.
fn named_key(named: Named) -> Option<&'static str> {
    NAMED_KEYS
        .iter()
        .find(|(key, _)| *key == named)
        .map(|(_, name)| *name)
}

/// Returns what nvim calls the key that types `character`, with Shift applied when Alt kept it
/// from being.
fn character_name(character: &str, modifiers: Modifiers) -> String {
    match character {
        "<" => "lt".to_owned(),
        " " => "Space".to_owned(),
        _ if modifiers.alt() && modifiers.shift() => character.to_uppercase(),
        _ => character.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use iced::keyboard::key::{NativeCode, Physical};
    use iced::keyboard::{Location, Modifiers};
    use sstack::vim::VimCmdline;

    use super::*;

    /// Returns a press of `key`, which types `modified` with every modifier but Ctrl applied, with
    /// `modifiers` held.
    fn press(key: Key, modified: Key, modifiers: Modifiers) -> keyboard::Event {
        keyboard::Event::KeyPressed {
            key,
            modified_key: modified,
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            location: Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        }
    }

    /// Returns a press of the key that types `character` without modifiers.
    fn character(character: &str, modifiers: Modifiers) -> keyboard::Event {
        let key = Key::Character(character.into());
        press(key.clone(), key, modifiers)
    }

    /// Returns a press of the key named `named`.
    fn named(named: Named, modifiers: Modifiers) -> keyboard::Event {
        press(Key::Named(named), Key::Named(named), modifiers)
    }

    #[test]
    fn keys_become_nvim_notation() {
        let shifted = press(
            Key::Character("a".into()),
            Key::Character("A".into()),
            Modifiers::SHIFT,
        );
        let option_c = press(
            Key::Character("c".into()),
            Key::Character("\u{e7}".into()),
            Modifiers::ALT,
        );
        let cases = [
            (character("a", Modifiers::empty()), "a"),
            (shifted, "A"),
            (character("<", Modifiers::empty()), "<lt>"),
            (character(" ", Modifiers::empty()), "<Space>"),
            (named(Named::Space, Modifiers::empty()), "<Space>"),
            (character("a", Modifiers::CTRL), "<C-a>"),
            (option_c, "<M-c>"),
            (character("j", Modifiers::ALT | Modifiers::SHIFT), "<M-J>"),
            (character("<", Modifiers::CTRL), "<C-lt>"),
            (named(Named::Enter, Modifiers::empty()), "<CR>"),
            (named(Named::Escape, Modifiers::empty()), "<Esc>"),
            (named(Named::F5, Modifiers::empty()), "<F5>"),
            (named(Named::Tab, Modifiers::SHIFT), "<S-Tab>"),
            (named(Named::ArrowLeft, Modifiers::SHIFT), "<S-Left>"),
        ];
        for (pressed, expected) in cases {
            assert_eq!(notation(&pressed).as_deref(), Some(expected), "{pressed:?}");
        }
    }

    #[test]
    fn keys_nvim_has_no_use_for_are_left_alone() {
        let released = keyboard::Event::ModifiersChanged(Modifiers::SHIFT);
        let unknown = press(Key::Unidentified, Key::Unidentified, Modifiers::empty());
        for ignored in [
            named(Named::PageUp, Modifiers::empty()),
            character("q", Modifiers::LOGO),
            released,
            unknown,
        ] {
            assert_eq!(notation(&ignored), None, "{ignored:?}");
        }
    }

    #[test]
    fn every_mode_has_a_label() {
        let modes = [
            VimMode::Normal,
            VimMode::Insert,
            VimMode::Replace,
            VimMode::Visual,
            VimMode::Command,
        ];
        let labels = modes.map(mode_label);
        assert_eq!(labels, ["NORMAL", "INSERT", "REPLACE", "VISUAL", "COMMAND"]);
    }

    #[test]
    fn rows_split_where_the_selection_starts_and_ends() {
        let rows = ["abc".to_owned(), "de".to_owned()];
        let split = runs(&rows, "abc\nde", std::slice::from_ref(&(1..5)));
        let run = |text: &str, selected| Run {
            text: text.to_owned(),
            selected,
        };
        let expected = [
            vec![run("a", false), run("bc", true)],
            vec![run("d", true), run("e", false)],
        ];
        assert_eq!(split, expected);
    }

    #[test]
    fn the_cmdline_cursor_sits_on_its_character_or_after_the_end() {
        let texts = |spans: Vec<Span<'_, ()>>| -> Vec<String> {
            spans.iter().map(|span| span.text.to_string()).collect()
        };
        assert_eq!(texts(cmdline_spans(":wq", 1)), [":", "w", "q"]);
        assert_eq!(texts(cmdline_spans(":wq", 3)), [":wq", " ", ""]);
    }

    /// Returns a prompt laid out 20 cells wide, showing `text` in `mode` with the cursor at the
    /// end. Its nvim is never given the chance to start, because the test's thread stays busy.
    fn shown(text: &str, mode: VimMode) -> VimPrompt {
        let mut prompt = VimPrompt::start(Draft::default());
        prompt.update(&VimEvent::State(VimState {
            text: text.to_owned(),
            cursor: text.len(),
            mode,
            selection: Vec::new(),
            line_numbers: Vec::new(),
            cmdline: None,
            message: None,
        }));
        let width = prompt.cell.width * 20.0;
        prompt.resized(Size::new(width, 50.0));
        prompt
    }

    /// Returns whether `prompt`, drawn, shows `text`. Saves a picture of it in the directory
    /// `SSTACK_GUI_PICTURES` names, if that is set, for a person to look at.
    fn draws(prompt: &VimPrompt, text: &str) -> bool {
        let mut ui = iced_test::simulator(prompt.view());
        if let Some(directory) = std::env::var_os("SSTACK_GUI_PICTURES") {
            let snapshot = ui.snapshot(&Theme::Light).unwrap();
            let name = format!("{text}-{}", prompt.state.text).replace(['\n', ' ', ':'], "_");
            let path = std::path::Path::new(&directory).join(name);
            let _ = snapshot.matches_image(path).unwrap();
        }
        ui.find(text).is_ok()
    }

    #[tokio::test]
    async fn the_prompt_shows_the_mode_and_wraps_at_its_width() {
        let normal = shown("a line long enough to wrap", VimMode::Normal);
        assert_eq!(normal.wrapped().rows.len(), 2);
        assert!(draws(&normal, "NORMAL"));
        for mode in [VimMode::Insert, VimMode::Replace, VimMode::Visual] {
            assert!(draws(&shown("hi", mode), mode_label(mode)));
        }
    }

    #[tokio::test]
    async fn nvim_is_told_the_size_once_the_prompt_is_laid_out() {
        let mut prompt = VimPrompt::start(Draft::default());
        assert_eq!(prompt.size, (0, 0));
        prompt.resized(Size::new(prompt.cell.width * 20.5, 50.0));
        assert_eq!(prompt.size, (1, 18));
    }

    #[tokio::test]
    async fn line_numbers_replace_the_marker_and_only_start_lines() {
        let mut prompt = shown("one\ntwo that wraps around", VimMode::Normal);
        prompt.state.line_numbers = vec!["1 ".into(), "2 ".into()];
        prompt.fit();
        let wrapped = prompt.wrapped();
        assert_eq!(prompt.lead(0, &wrapped.line_starts), "1 ");
        assert_eq!(prompt.lead(1, &wrapped.line_starts), "2 ");
        assert_eq!(prompt.lead(2, &wrapped.line_starts), "  ");
        assert_eq!(prompt.size.1, 20);
        assert!(draws(&prompt, "NORMAL"));
    }

    #[tokio::test]
    async fn the_command_line_and_errors_show_under_the_text() {
        let mut prompt = shown("hi", VimMode::Command);
        prompt.state.cmdline = Some(VimCmdline {
            text: ":wq".into(),
            cursor: 3,
        });
        assert!(prompt.cursor((0, 0)).is_none());
        assert!(draws(&prompt, "COMMAND"));
        prompt.state.cmdline = None;
        prompt.state.message = Some("E492: Not an editor command".into());
        assert!(draws(&prompt, "E492: Not an editor command"));
    }

    #[tokio::test]
    async fn submitting_empties_the_draft_at_once() {
        let mut prompt = shown("hi", VimMode::Insert);
        prompt.update(&VimEvent::Submitted("hi".into()));
        assert_eq!(prompt.draft(), Draft::default());
    }

    #[test]
    fn a_cell_has_room_for_one_character() {
        let cell = cell_size();
        assert!(cell.width > 0.0 && cell.width < cell.height, "{cell:?}");
    }
}
