//! The layout of text being typed, so that every way of typing looks the same.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::tui::theme;

/// The marker shown before the first row of what the person types.
const MARKER: &str = "> ";

/// The indent shown before every later row, so wrapped text lines up after the marker.
const INDENT: &str = "  ";

/// How many columns the marker and indent take.
pub const GUTTER: usize = MARKER.len();

/// How the border around the input adds to its height and width.
pub const BORDER: u16 = 2;

/// Text laid out to fit a width.
pub struct Wrapped {
    /// Each row on screen.
    pub rows: Vec<String>,
    /// The row and column the cursor is at.
    pub cursor: (usize, usize),
    /// Which row each line of the text starts on, in order. A line can take several rows.
    pub line_starts: Vec<usize>,
}

/// Lays out `text` in rows of `columns` columns, finding the byte offset `cursor` in them.
pub fn wrap_to(text: &str, cursor: usize, columns: usize) -> Wrapped {
    let (before, after) = text.split_at(cursor);
    let mut rows = Rows::new(columns);
    before.chars().for_each(|character| rows.push(character));
    let position = rows.position();
    after.chars().for_each(|character| rows.push(character));
    let line_starts = rows.line_starts.clone();
    Wrapped {
        rows: rows.finish(),
        cursor: position,
        line_starts,
    }
}

/// Returns `text` broken into rows that fit `width` columns after the gutter, and the row and
/// column of the byte offset `cursor`.
pub fn wrap(text: &str, cursor: usize, width: u16) -> (Vec<String>, (usize, usize)) {
    let columns = usize::from(width).saturating_sub(GUTTER);
    let wrapped = wrap_to(text, cursor, columns);
    (wrapped.rows, wrapped.cursor)
}

/// Returns the row at `index`, already styled, with the marker or indent before it.
pub fn gutter(index: usize, row: Vec<Span<'static>>) -> Line<'static> {
    let lead = if index == 0 { MARKER } else { INDENT };
    let mut spans = vec![Span::styled(lead, theme::MARKER)];
    spans.extend(row);
    Line::from(spans)
}

/// Returns `text` with every line break as a plain newline, since terminals paste them as
/// carriage returns, which would garble the screen.
pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Converts a count to screen units, saturating rather than wrapping when it is too big.
pub fn to_u16(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// A layout of text into rows of a fixed width, one character at a time, breaking at newlines and
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
    /// Which row each line of the text starts on.
    line_starts: Vec<usize>,
}

impl Rows {
    /// Starts laying out rows `width` columns wide.
    fn new(width: usize) -> Self {
        Self {
            full: Vec::new(),
            row: String::new(),
            width: width.max(1),
            column: 0,
            line_starts: vec![0],
        }
    }

    /// Adds `character`, starting a new row first if it would not fit.
    fn push(&mut self, character: char) {
        if character == '\n' {
            self.break_row();
            self.line_starts.push(self.full.len());
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
