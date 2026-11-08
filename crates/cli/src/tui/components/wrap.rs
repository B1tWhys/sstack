//! The layout of text being typed, so that every way of typing looks the same.

use ratatui::text::{Line, Span};
use sstack::vim::layout::wrap_to;

use crate::tui::theme;

/// The marker shown before the first row of what the person types.
const MARKER: &str = "> ";

/// The indent shown before every later row, so wrapped text lines up after the marker.
const INDENT: &str = "  ";

/// How many columns the marker and indent take.
pub const GUTTER: usize = MARKER.len();

/// How the border around the input adds to its height and width.
pub const BORDER: u16 = 2;

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
