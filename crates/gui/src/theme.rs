//! The colors the window is drawn in. They come from one palette of five, plus a red for errors,
//! so changing the constants here recolors the whole window.
//!
//! Widgets left to iced's own styles take their colors from [`theme`]. The styles here cover the
//! parts that use a palette color iced has no role for.

// Colors are written as the hex codes color pickers show, so they can be pasted in and out.
#![expect(clippy::unreadable_literal)]

use iced::theme::Palette;
use iced::theme::palette::{Pair, deviate, mix};
use iced::widget::{button, container, text};
use iced::{Border, Color, Theme, color};

/// The window's background, near black.
const BACKGROUND: Color = color!(0x000505);

/// The color of text, near white.
const TEXT: Color = color!(0xfefcfd);

/// The color of buttons, of the Session on screen in the sidebar and of anything else iced draws
/// as the main action, a muted violet.
const PRIMARY: Color = color!(0x5d5d81);

/// The background of panels that stand apart from the window's background, such as the prompt's
/// box, what the person said in the transcript, and the Sessions in the sidebar other than the
/// one on screen, a deep purple.
const SURFACE: Color = color!(0x3b3355);

/// The color of the prompt's cursor and of selected text, a pale blue. Drawn see-through, so the
/// text under it stays readable.
pub const HIGHLIGHT: Color = color!(0xbfcde0);

/// The color of errors and of Sessions that need the person, a light red. The palette has no
/// red, and this one is light enough to read on the background and on a panel.
const DANGER: Color = color!(0xff7b7b);

/// How far muted text fades toward the background, as a fraction of the way. The fade is in
/// linear light, so it looks less faded than the number suggests.
const MUTED_FADE: f32 = 0.7;

/// How round the corners of a panel are, in logical pixels.
const CORNER_RADIUS: f32 = 4.0;

/// How much lighter a panel-colored button turns while the pointer is over it, as a fraction
/// toward white.
const HOVER_LIFT: f32 = 0.1;

/// Returns the theme the window is drawn in: a dark one built from the palette. It does not
/// follow the system's light or dark setting.
pub fn theme() -> Theme {
    Theme::custom(
        "sstack",
        Palette {
            background: BACKGROUND,
            text: TEXT,
            primary: PRIMARY,
            danger: DANGER,
            ..Palette::DARK
        },
    )
}

/// Returns the style of a panel with round corners that stands apart from the window's
/// background, such as the prompt's box and what the person said in the transcript.
pub fn panel(_: &Theme) -> container::Style {
    let pair = Pair::new(SURFACE, TEXT);
    container::Style::default()
        .background(pair.color)
        .color(pair.text)
        .border(Border::default().rounded(CORNER_RADIUS))
}

/// Returns the style of an open menu: a panel with an edge in the main color, so that it stands
/// apart from the bar it hangs from, which shares its color.
pub fn menu(theme: &Theme) -> container::Style {
    panel(theme).border(
        Border::default()
            .rounded(CORNER_RADIUS)
            .width(1)
            .color(PRIMARY),
    )
}

/// Returns the style of text that matters less than what is around it, such as what the model
/// thought or said on its way to an answer. It is dimmer than other text but still readable.
pub fn muted(_: &Theme) -> text::Style {
    text::Style {
        color: Some(muted_color()),
    }
}

/// Returns the style of a container whose text is all muted, for text drawn by a widget that
/// takes no text style of its own, such as Markdown.
pub fn faded(_: &Theme) -> container::Style {
    container::Style::default().color(muted_color())
}

/// Returns the color of muted text.
fn muted_color() -> Color {
    mix(TEXT, BACKGROUND, MUTED_FADE)
}

/// Returns the style of a button that is available but not the focus, in its `status`, such as a
/// Session in the sidebar other than the one on screen. It is drawn in the panel color.
pub fn quiet(theme: &Theme, status: button::Status) -> button::Style {
    let color = if status == button::Status::Hovered {
        deviate(SURFACE, HOVER_LIFT)
    } else {
        SURFACE
    };
    let pair = Pair::new(color, TEXT);
    button::Style {
        background: Some(pair.color.into()),
        text_color: pair.text,
        ..button::secondary(theme, status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contrast WCAG asks of normal text, as a ratio of luminances.
    const READABLE: f32 = 4.5;

    #[test]
    fn text_reads_on_everything_it_is_drawn_on() {
        let drawn = [
            (TEXT, BACKGROUND),
            (TEXT, PRIMARY),
            (TEXT, SURFACE),
            (TEXT, deviate(SURFACE, HOVER_LIFT)),
            (muted_color(), BACKGROUND),
            (DANGER, BACKGROUND),
            (DANGER, SURFACE),
        ];
        for (text, background) in drawn {
            let contrast = text.relative_contrast(background);
            assert!(
                contrast >= READABLE,
                "{text:?} on {background:?} has a contrast of {contrast}"
            );
        }
    }

    #[test]
    fn iced_draws_text_in_the_palettes_own_color() {
        let theme = theme();
        let palette = theme.extended_palette();
        for pair in [palette.background.base, palette.primary.base] {
            assert_eq!(pair.text, TEXT);
        }
    }
}
