//! The layout of text on a grid of fixed-width cells, as nvim lays it out, so that a front end
//! drawing a [`VimState`](super::VimState) wraps at the same columns nvim moves the cursor by.

use unicode_width::UnicodeWidthChar;

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
#[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_breaks_at_newlines_and_where_a_row_is_full() {
        let wrapped = wrap_to("abcde\nf", 7, 3);
        assert_eq!(wrapped.rows, ["abc", "de", "f"]);
        assert_eq!(wrapped.line_starts, [0, 2]);
        assert_eq!(wrapped.cursor, (2, 1));
    }

    #[test]
    fn a_cursor_after_a_full_row_starts_the_next() {
        let wrapped = wrap_to("abc", 3, 3);
        assert_eq!(wrapped.rows, ["abc", ""]);
        assert_eq!(wrapped.cursor, (1, 0));
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let wrapped = wrap_to("\u{4e00}\u{4e00}", 0, 3);
        assert_eq!(wrapped.rows, ["\u{4e00}", "\u{4e00}"]);
    }
}
