//! The one place in a Session where the person types, however they have chosen to.

use iced::Size;
use iced::widget::text_editor::{self, Cursor, Position};
use sstack::vim::{Draft, VimEvent};

use crate::vim_prompt::{KeyOutcome, VimPrompt};

/// The way the person types in a Session, which switches with `/vim` and falls back to the plain
/// prompt by itself if nvim is lost.
pub enum Input {
    /// The plain prompt, an ordinary text box.
    Plain(text_editor::Content),
    /// A real nvim.
    Vim(VimPrompt),
}

impl Default for Input {
    fn default() -> Self {
        Self::Plain(text_editor::Content::new())
    }
}

impl Input {
    /// Returns the plain prompt holding `draft`, with its cursor where it was.
    pub fn plain(draft: &Draft) -> Self {
        let mut content = text_editor::Content::with_text(&draft.text);
        content.move_to(Cursor {
            position: position(&draft.text, draft.cursor),
            selection: None,
        });
        Self::Plain(content)
    }

    /// Returns the text so far, with its cursor.
    pub fn draft(&self) -> Draft {
        match self {
            Self::Plain(content) => {
                let text = content.text();
                let cursor = byte_offset(&text, content.cursor().position);
                Draft { text, cursor }
            }
            Self::Vim(vim) => vim.draft(),
        }
    }

    /// Starts typing into a real nvim, keeping what is typed. Already typing into one, nothing
    /// changes.
    ///
    /// Must be called inside a Tokio runtime, which runs nvim's side of the conversation.
    pub fn start_vim(&mut self) {
        if let Self::Plain(_) = self {
            *self = Self::Vim(VimPrompt::start(self.draft()));
        }
    }

    /// Switches between the two ways of typing, keeping what is typed.
    pub fn toggle(&mut self) {
        match self {
            Self::Plain(_) => self.start_vim(),
            Self::Vim(vim) => *self = Self::plain(&vim.draft()),
        }
    }

    /// Makes the change the person asked of the plain prompt. Nvim does its own editing.
    pub fn perform(&mut self, action: text_editor::Action) {
        if let Self::Plain(content) = self {
            content.perform(action);
        }
    }

    /// Takes what the person typed into the plain prompt, leaving it empty. Nvim instead hands
    /// its text back later as [`VimEvent::Submitted`], so this asks it to and returns `None`.
    pub fn take(&mut self) -> Option<text_editor::Content> {
        match self {
            Self::Plain(content) => Some(std::mem::take(content)),
            Self::Vim(vim) => {
                vim.submit();
                None
            }
        }
    }

    /// Puts `typed` back after what the person sent was refused: in place of the plain prompt,
    /// or pasted back into nvim, which has emptied itself by then.
    pub fn restore(&mut self, typed: text_editor::Content) {
        match self {
            Self::Plain(_) => *self = Self::Plain(typed),
            Self::Vim(vim) => vim.paste(&typed.text()),
        }
    }

    /// Offers nvim a key the person pressed, returning what the window still has to do about
    /// it. The plain prompt hears keys through its own widget instead.
    pub fn key(&self, event: &iced::keyboard::Event) -> KeyOutcome {
        match self {
            Self::Plain(_) => KeyOutcome::Ignored,
            Self::Vim(vim) => vim.key(event),
        }
    }

    /// Pastes `pasted`, what the clipboard held, into nvim, if the person still types into it.
    pub fn paste(&self, pasted: Option<String>) {
        if let (Self::Vim(vim), Some(pasted)) = (self, pasted) {
            vim.paste(&pasted);
        }
    }

    /// Notes that the window laid out nvim's prompt at `size`, in logical pixels.
    pub fn resized(&mut self, size: Size) {
        if let Self::Vim(vim) = self {
            vim.resized(size);
        }
    }

    /// Shows what nvim reported, returning whether it was taken. A report that comes from an nvim
    /// the person already switched away from is not. Losing nvim falls back to the plain prompt
    /// with the text so far.
    pub fn vim_reported(&mut self, event: &VimEvent) -> bool {
        let Self::Vim(vim) = self else {
            return false;
        };
        vim.update(event);
        if let VimEvent::Lost { draft, .. } = event {
            *self = Self::plain(draft);
        }
        true
    }
}

/// Returns where the byte offset `offset` into `text` is, as a line and a byte offset into it.
fn position(text: &str, offset: usize) -> Position {
    let before = &text[..offset.min(text.len())];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Position {
        line: before.matches('\n').count(),
        column: before.len() - line_start,
    }
}

/// Returns the byte offset into `text` of `position`, a line and a byte offset into it. A
/// position past the end of its line or of the text is moved back to the end.
fn byte_offset(text: &str, position: Position) -> usize {
    let mut lines = text.split('\n');
    let before: usize = lines
        .by_ref()
        .take(position.line)
        .map(|line| line.len() + 1)
        .sum();
    let line = lines.next().unwrap_or_default();
    (before + position.column.min(line.len())).min(text.len())
}

#[cfg(test)]
mod tests {
    use sstack::vim::VimFailure;

    use super::*;

    #[test]
    fn a_draft_keeps_its_cursor_in_the_plain_prompt() {
        for (text, cursor) in [("", 0), ("one\ntwo", 5), ("one\n", 4), ("\u{e9}t\u{e9}", 3)] {
            let draft = Draft {
                text: text.into(),
                cursor,
            };
            assert_eq!(Input::plain(&draft).draft(), draft, "{text:?} at {cursor}");
        }
    }

    #[test]
    fn a_position_past_the_end_of_its_line_is_moved_back() {
        let past = Position { line: 0, column: 9 };
        assert_eq!(byte_offset("one\ntwo", past), 3);
        let missing = Position { line: 5, column: 0 };
        assert_eq!(byte_offset("one", missing), 3);
    }

    #[tokio::test]
    async fn switching_keeps_the_text_both_ways() {
        let mut input = Input::plain(&Draft {
            text: "kept".into(),
            cursor: 2,
        });
        input.toggle();
        assert!(matches!(input, Input::Vim(_)));
        assert_eq!(input.draft().text, "kept");
        input.toggle();
        assert!(matches!(input, Input::Plain(_)));
        assert_eq!(input.draft().text, "kept");
    }

    #[tokio::test]
    async fn losing_nvim_falls_back_to_the_plain_prompt() {
        let mut input = Input::default();
        let lost = VimEvent::Lost {
            draft: Draft {
                text: "so far".into(),
                cursor: 2,
            },
            failure: VimFailure::TimedOut,
        };
        assert!(!input.vim_reported(&lost));
        input.start_vim();
        assert!(input.vim_reported(&lost));
        assert!(matches!(input, Input::Plain(_)));
        assert_eq!(input.draft().cursor, 2);
    }

    #[tokio::test]
    async fn the_plain_prompt_leaves_keys_pastes_and_layouts_to_its_widget() {
        let mut input = Input::plain(&Draft {
            text: "kept".into(),
            cursor: 0,
        });
        let key = iced::keyboard::Event::ModifiersChanged(iced::keyboard::Modifiers::empty());
        assert_eq!(input.key(&key), KeyOutcome::Ignored);
        input.paste(Some("pasted".into()));
        input.resized(Size::new(100.0, 10.0));
        assert_eq!(input.draft().text, "kept");
    }
}
