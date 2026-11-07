//! Editing text with a cursor, independent of which keys drive it.

use std::ops::Range;

/// Edits text the way a shell's command line does: moving by character, word and line, deleting,
/// and killing text so it can be yanked back.
///
/// It knows nothing of keys, so different key schemes can drive the same edits. A line is the text
/// between newlines, not a row on screen.
#[derive(Debug, Default)]
pub struct Editor {
    /// Everything typed so far.
    text: String,
    /// Where the next character goes, as a byte offset into `text` that is always on a character
    /// boundary.
    cursor: usize,
    /// The most recently killed text, which a yank inserts. Survives [`Editor::take`].
    killed: String,
}

/// One change to an [`Editor`] that needs nothing but the editor, so that a key scheme can bind
/// keys to edits in a table.
pub type Edit = fn(&mut Editor);

/// Tells whether a character belongs to a word: letters and digits, so punctuation separates
/// words.
fn word(character: char) -> bool {
    character.is_alphanumeric()
}

/// Tells whether a character belongs to a WORD in vim's sense: anything but whitespace.
fn big_word(character: char) -> bool {
    !character.is_whitespace()
}

impl Editor {
    /// Returns the text being edited.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns where the cursor sits, as a byte offset into [`Editor::text`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the text and empties the editor, keeping what was last killed.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Inserts `character` before the cursor.
    pub fn insert(&mut self, character: char) {
        self.text.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    /// Inserts `text` before the cursor, leaving the cursor after it.
    pub fn insert_str(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// Moves the cursor one character left.
    pub fn move_left(&mut self) {
        self.cursor = self.previous_char();
    }

    /// Moves the cursor one character right.
    pub fn move_right(&mut self) {
        self.cursor = self.next_char();
    }

    /// Moves the cursor to the start of its line.
    pub fn move_line_start(&mut self) {
        self.cursor = self.line_start();
    }

    /// Moves the cursor to the end of its line.
    pub fn move_line_end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Moves the cursor to the start of the word it is in, or of the previous word.
    pub fn move_word_left(&mut self) {
        self.cursor = self.word_start(word);
    }

    /// Moves the cursor to the end of the word it is in, or of the next word.
    pub fn move_word_right(&mut self) {
        self.cursor = self.word_end(word);
    }

    /// Deletes the character before the cursor.
    pub fn delete_left(&mut self) {
        self.remove(self.previous_char()..self.cursor);
    }

    /// Deletes the character under the cursor.
    pub fn delete_right(&mut self) {
        self.remove(self.cursor..self.next_char());
    }

    /// Kills from the start of the line to the cursor.
    pub fn kill_line_start(&mut self) {
        self.kill(self.line_start()..self.cursor);
    }

    /// Kills from the cursor to the end of the line.
    pub fn kill_line_end(&mut self) {
        self.kill(self.cursor..self.line_end());
    }

    /// Kills back to the start of the word before the cursor, stopping at punctuation.
    pub fn kill_word_left(&mut self) {
        self.kill(self.word_start(word)..self.cursor);
    }

    /// Kills back to the previous whitespace, taking punctuation with it.
    pub fn kill_big_word_left(&mut self) {
        self.kill(self.word_start(big_word)..self.cursor);
    }

    /// Kills forward to the end of the word after the cursor.
    pub fn kill_word_right(&mut self) {
        self.kill(self.cursor..self.word_end(word));
    }

    /// Inserts the most recently killed text before the cursor.
    pub fn yank(&mut self) {
        let killed = self.killed.clone();
        self.insert_str(&killed);
    }

    /// Returns where the character before the cursor starts, or the cursor at the start.
    fn previous_char(&self) -> usize {
        let before = self.text[..self.cursor].char_indices().next_back();
        before.map_or(0, |(index, _)| index)
    }

    /// Returns where the character after the cursor's ends, or the cursor at the end.
    fn next_char(&self) -> usize {
        let after = self.text[self.cursor..].chars().next();
        after.map_or(self.cursor, |character| self.cursor + character.len_utf8())
    }

    /// Returns where the cursor's line starts.
    fn line_start(&self) -> usize {
        let newline = self.text[..self.cursor].rfind('\n');
        newline.map_or(0, |index| index + 1)
    }

    /// Returns where the cursor's line ends, before its newline.
    fn line_end(&self) -> usize {
        let newline = self.text[self.cursor..].find('\n');
        newline.map_or(self.text.len(), |index| self.cursor + index)
    }

    /// Returns where the word before the cursor starts, skipping anything between the two.
    fn word_start(&self, is_word: fn(char) -> bool) -> usize {
        let before = &self.text[..self.cursor];
        let to_word = before.trim_end_matches(|character| !is_word(character));
        to_word.trim_end_matches(is_word).len()
    }

    /// Returns where the word after the cursor ends, skipping anything between the two.
    fn word_end(&self, is_word: fn(char) -> bool) -> usize {
        let after = &self.text[self.cursor..];
        let to_word = after.trim_start_matches(|character| !is_word(character));
        self.text.len() - to_word.trim_start_matches(is_word).len()
    }

    /// Removes `range` so that it can be yanked back. Killing nothing keeps the earlier kill.
    fn kill(&mut self, range: Range<usize>) {
        let removed = self.remove(range);
        if !removed.is_empty() {
            self.killed = removed;
        }
    }

    /// Removes `range` from the text, leaving the cursor where it started, and returns it.
    fn remove(&mut self, range: Range<usize>) -> String {
        self.cursor = range.start;
        self.text.drain(range).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns an editor holding `text`, with the cursor where `|` is.
    fn editor(text: &str) -> Editor {
        let cursor = text.find('|').unwrap();
        Editor {
            text: text.replacen('|', "", 1),
            cursor,
            killed: String::new(),
        }
    }

    /// Returns the editor's text with `|` where the cursor is.
    fn shown(editor: &Editor) -> String {
        let mut text = editor.text().to_owned();
        text.insert(editor.cursor(), '|');
        text
    }

    /// Applies `edit` to an editor holding `before`, and returns what it then holds.
    fn after(before: &str, edit: fn(&mut Editor)) -> String {
        let mut editor = editor(before);
        edit(&mut editor);
        shown(&editor)
    }

    #[test]
    fn typing_goes_in_at_the_cursor() {
        let mut editor = editor("hé|o");
        editor.insert('l');
        editor.insert_str("l");
        assert_eq!(shown(&editor), "héll|o");
    }

    #[test]
    fn the_cursor_moves_by_character_without_leaving_the_text() {
        assert_eq!(after("hé|", Editor::move_left), "h|é");
        assert_eq!(after("|hi", Editor::move_left), "|hi");
        assert_eq!(after("h|é", Editor::move_right), "hé|");
        assert_eq!(after("hi|", Editor::move_right), "hi|");
    }

    #[test]
    fn the_cursor_moves_to_either_end_of_its_own_line() {
        assert_eq!(
            after("one\ntw|o\nthree", Editor::move_line_start),
            "one\n|two\nthree"
        );
        assert_eq!(after("on|e", Editor::move_line_start), "|one");
        assert_eq!(
            after("one\nt|wo\nthree", Editor::move_line_end),
            "one\ntwo|\nthree"
        );
        assert_eq!(after("o|ne", Editor::move_line_end), "one|");
    }

    #[test]
    fn the_cursor_moves_by_word_over_punctuation() {
        assert_eq!(
            after("cargo test --wor|k", Editor::move_word_left),
            "cargo test --|work"
        );
        assert_eq!(
            after("cargo test --|work", Editor::move_word_left),
            "cargo |test --work"
        );
        assert_eq!(after("  |hi", Editor::move_word_left), "|  hi");
        assert_eq!(after("ca|rgo test", Editor::move_word_right), "cargo| test");
        assert_eq!(
            after("cargo| --test", Editor::move_word_right),
            "cargo --test|"
        );
    }

    #[test]
    fn deleting_takes_one_character_either_side() {
        assert_eq!(after("hé|o", Editor::delete_left), "h|o");
        assert_eq!(after("|ho", Editor::delete_left), "|ho");
        assert_eq!(after("h|éo", Editor::delete_right), "h|o");
        assert_eq!(after("ho|", Editor::delete_right), "ho|");
    }

    #[test]
    fn killing_removes_to_line_and_word_boundaries() {
        assert_eq!(after("a\nbc|d\ne", Editor::kill_line_start), "a\n|d\ne");
        assert_eq!(after("a\nb|cd\ne", Editor::kill_line_end), "a\nb|\ne");
        assert_eq!(
            after("git add src/ma|in", Editor::kill_word_left),
            "git add src/|in"
        );
        assert_eq!(
            after("git add src/ma|in", Editor::kill_big_word_left),
            "git add |in"
        );
        assert_eq!(after("git| add src", Editor::kill_word_right), "git| src");
    }

    #[test]
    fn a_yank_brings_back_the_last_thing_killed() {
        let mut editor = editor("one two|");
        editor.kill_word_left();
        editor.kill_line_end();
        editor.move_line_start();
        editor.yank();
        assert_eq!(shown(&editor), "two|one ");
    }

    #[test]
    fn taking_the_text_empties_the_editor_but_keeps_the_kill() {
        let mut editor = editor("keep |this");
        editor.kill_line_end();
        assert_eq!(editor.take(), "keep ");
        assert_eq!(shown(&editor), "|");
        editor.yank();
        assert_eq!(shown(&editor), "this|");
    }
}
