//! The model's reply in one Step, drawn as Markdown.

use std::fmt;

use iced::theme::Palette;
use iced::widget::{container, markdown};
use iced::{Element, Pixels};

use super::{ASIDE_SIZE, TEXT_SIZE};
use crate::theme;

/// The model's reply in one Step, as much of it as has arrived, drawn with its Markdown
/// formatting rather than as the marks that ask for it. Code blocks are colored for the language
/// they name, if it is one iced knows. Images show as their alternative text.
///
/// Two replies are equal when their text is, however it arrived.
pub struct Reply {
    /// The text as the model sent it.
    source: String,
    /// The text read as Markdown, kept up to date as pieces arrive so that a long reply is not
    /// read again from the start each time. Boxed because it is large, and a transcript block
    /// holding it inline would make every other kind of block as large.
    markdown: Box<markdown::Content>,
}

impl Reply {
    /// Adds `piece` to the end of the reply, as the model streams it.
    pub fn push_str(&mut self, piece: &str) {
        self.source.push_str(piece);
        self.markdown.push_str(piece);
    }

    /// Returns the reply as the model sent it, with the marks that ask for Markdown formatting.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Draws the reply. Clicking a link sends what `open` makes of its address.
    pub fn view<'a, Message: 'a>(&'a self, open: fn(String) -> Message) -> Element<'a, Message> {
        self.drawn(Pixels(TEXT_SIZE), open)
    }

    /// Draws the reply as an aside, smaller and dimmer than an answer, for a reply the model gave
    /// on its way to one. Clicking a link sends what `open` makes of its address.
    pub fn aside<'a, Message: 'a>(&'a self, open: fn(String) -> Message) -> Element<'a, Message> {
        container(self.drawn(Pixels(ASIDE_SIZE), open))
            .style(theme::faded)
            .into()
    }

    /// Draws the reply with body text `size` big, and headings in proportion. Clicking a link
    /// sends what `open` makes of its address.
    fn drawn<'a, Message: 'a>(
        &'a self,
        size: Pixels,
        open: fn(String) -> Message,
    ) -> Element<'a, Message> {
        markdown::view(self.markdown.items(), settings(size)).map(open)
    }
}

impl From<&str> for Reply {
    fn from(text: &str) -> Self {
        Self {
            source: text.to_owned(),
            markdown: Box::new(markdown::Content::parse(text)),
        }
    }
}

impl PartialEq for Reply {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Reply {}

impl fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Reply").field(&self.source).finish()
    }
}

/// Returns the sizes and style replies are drawn in, with body text `size` big. Code blocks are
/// drawn as large as the words around them, where iced would draw them at three quarters of that
/// size.
fn settings(size: Pixels) -> markdown::Settings {
    markdown::Settings {
        code_size: size,
        ..markdown::Settings::with_text_size(size, style())
    }
}

/// Returns how replies are drawn. The palette only gives the color of links, which take the blue
/// of iced's light palette so they stand out from the text around them. The window's own main
/// color is too dark to read as text on its background.
fn style() -> markdown::Style {
    markdown::Style::from_palette(Palette::LIGHT)
}

#[cfg(test)]
mod tests {
    use iced::font::Weight;
    use iced::widget::markdown::Item;

    use super::*;

    /// Returns the reply that arrives in `pieces`, in order.
    fn streamed(pieces: &[&str]) -> Reply {
        let mut reply = Reply::from("");
        for piece in pieces {
            reply.push_str(piece);
        }
        reply
    }

    /// Returns what kind of Markdown each part of `reply` is.
    fn kinds(reply: &Reply) -> Vec<&'static str> {
        let kind = |item: &Item| match item {
            Item::Heading(..) => "heading",
            Item::Paragraph(_) => "paragraph",
            Item::CodeBlock { .. } => "code",
            Item::List { .. } => "list",
            _ => "other",
        };
        reply.markdown.items().iter().map(kind).collect()
    }

    /// Returns the words of `reply`'s only paragraph that `keep` picks out by how they are drawn.
    fn words_where(
        reply: &Reply,
        keep: impl Fn(&iced::advanced::text::Span<'_, String>) -> bool,
    ) -> Vec<String> {
        let [Item::Paragraph(text)] = reply.markdown.items() else {
            panic!("the reply should be one paragraph");
        };
        let spans = text.spans(style());
        spans
            .iter()
            .filter(|span| keep(span))
            .map(|span| span.text.to_string())
            .collect()
    }

    #[test]
    fn a_streamed_reply_is_read_as_markdown_however_it_was_split() {
        let reply = streamed(&[
            "# Pl",
            "an\n\n- one\n- tw",
            "o\n\n```rust\nlet x",
            " = 1;\n```\n",
        ]);

        assert_eq!(kinds(&reply), ["heading", "list", "code"]);
        assert_eq!(
            reply,
            Reply::from("# Plan\n\n- one\n- two\n\n```rust\nlet x = 1;\n```\n")
        );
    }

    #[test]
    fn emphasis_split_across_pieces_is_drawn_bold() {
        let reply = streamed(&["some **bo", "ld** words"]);

        let bold = words_where(&reply, |span| {
            span.font.is_some_and(|font| font.weight == Weight::Bold)
        });
        assert_eq!(bold, ["bold"]);
    }

    #[test]
    fn a_code_block_is_drawn_in_the_colors_of_its_language() {
        let reply = Reply::from("```rust\nfn main() { let x = \"one\"; }\n```\n");

        let [Item::CodeBlock { lines, .. }] = reply.markdown.items() else {
            panic!("the reply should be one code block");
        };
        let mut colors: Vec<_> = lines
            .iter()
            .flat_map(|line| line.spans(style()).to_vec())
            .filter_map(|span| span.color)
            .map(|color| format!("{color:?}"))
            .collect();
        colors.dedup();
        assert!(colors.len() > 1, "only drawn in {colors:?}");
    }

    #[test]
    fn code_in_a_block_is_as_large_as_the_words_around_it() {
        for size in [TEXT_SIZE, ASIDE_SIZE] {
            let settings = settings(Pixels(size));

            assert_eq!(settings.code_size, settings.text_size);
        }
    }

    #[test]
    fn a_link_is_drawn_as_its_words_and_leads_to_its_address() {
        let reply = Reply::from("see [the docs](https://example.com) here");

        let links = words_where(&reply, |span| {
            span.link.as_deref() == Some("https://example.com")
        });
        assert_eq!(links, ["the docs"]);
    }
}
