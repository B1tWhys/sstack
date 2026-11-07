//! The look of the interactive screen, kept in one place so it can be changed without touching
//! any component.
//!
//! Components use only the role styles, named for what they mark. The palette below them names
//! each color for how it looks, so a role can move to another color, or a color can be adjusted
//! everywhere at once. The colors are soft and light, meant for a dark terminal.
//!
//! Colors are written as exact 24-bit values. On terminals that cannot show those, the App draws
//! each one as the nearest color the terminal has, using [`ColorDepth`].

use anstyle::{Ansi256Color, AnsiColor, RgbColor};
use anstyle_lossy::palette;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

/// A pale, soft clay, warm and easy to read at length. Leans pinker than sand.
const CLAY: Color = Color::Rgb(216, 192, 178);

/// A warm off-white, yellower than clay, so it can be told apart from it without drawing the eye.
const SAND: Color = Color::Rgb(222, 212, 192);

/// A muted warm gray, readable but clearly secondary.
const TAUPE: Color = Color::Rgb(146, 136, 126);

/// A dark warm gray that recedes, for detail most people skim past.
const ASH: Color = Color::Rgb(122, 114, 106);

/// A warm gray just lighter than a dark background, for frames that should barely show.
const SHADOW: Color = Color::Rgb(86, 78, 72);

/// A soft amber that asks for attention without alarm.
const AMBER: Color = Color::Rgb(206, 176, 124);

/// A warm background a shade lighter than a dark terminal, for a bar that stands out quietly.
const RAISED: Color = Color::Rgb(50, 44, 40);

/// The model's replies, and other text meant to be read in full.
pub const TEXT: Style = Style::new().fg(CLAY);

/// What the person sent, as shown in the transcript. Bold, so each Turn is easy to find.
pub const PROMPT: Style = Style::new().fg(SAND).add_modifier(Modifier::BOLD);

/// What the person is typing and has not sent yet.
pub const INPUT: Style = Style::new().fg(SAND);

/// The marker before what the person types.
pub const MARKER: Style = Style::new().fg(TAUPE);

/// The model's summarized thinking, which is there to skim.
pub const REASONING: Style = Style::new().fg(TAUPE).add_modifier(Modifier::ITALIC);

/// Tool activity, warnings, and messages from the harness itself.
pub const NOTE: Style = Style::new().fg(ASH);

/// The frame around the prompt.
pub const BORDER: Style = Style::new().fg(SHADOW);

/// The line telling the person where they are and what they can do.
pub const STATUS: Style = Style::new().fg(TAUPE).bg(RAISED);

/// The frame and title of a question the person must answer before anything else happens.
pub const ATTENTION: Style = Style::new().fg(AMBER);

/// The 16 basic terminal colors in their standard order, so that a color's number picks its name.
/// Named colors are drawn with codes that even the most limited terminals understand.
const BASIC: [Color; 16] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Gray,
    Color::DarkGray,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// How many colors the terminal can show, which decides how the theme's colors are drawn.
///
/// `NO_COLOR` needs no handling here, because the terminal library drops all color when it is
/// set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    /// Any 24-bit color, so the theme is shown exactly.
    TrueColor,
    /// The standard 256-color palette.
    Ansi256,
    /// Only the 16 basic colors, whose look depends on the person's terminal theme, so matches
    /// are rough.
    Ansi16,
}

impl ColorDepth {
    /// Returns what the terminal reports through `COLORTERM` and `TERM`, assuming the 16 basic
    /// colors when neither says more. Setting `COLORTERM=truecolor` forces exact colors.
    pub fn detect() -> Self {
        Self::from_count(crossterm::style::available_color_count())
    }

    /// Returns the depth for a terminal that can show `count` colors, where `u16::MAX` stands
    /// for any 24-bit color.
    fn from_count(count: u16) -> Self {
        match count {
            u16::MAX => Self::TrueColor,
            256.. => Self::Ansi256,
            _ => Self::Ansi16,
        }
    }

    /// Changes every color drawn into `buffer` to the nearest one this depth can show.
    pub fn fit_buffer(self, buffer: &mut Buffer) {
        for cell in &mut buffer.content {
            cell.fg = self.fit(cell.fg);
            cell.bg = self.fit(cell.bg);
        }
    }

    /// Returns the nearest color to `color` that this depth can show. Only 24-bit colors change.
    fn fit(self, color: Color) -> Color {
        let Color::Rgb(red, green, blue) = color else {
            return color;
        };
        let rgb = RgbColor(red, green, blue);
        match self {
            Self::TrueColor => color,
            Self::Ansi256 => Color::Indexed(anstyle_lossy::rgb_to_xterm(rgb).index()),
            Self::Ansi16 => basic(anstyle_lossy::rgb_to_ansi(rgb, palette::DEFAULT)),
        }
    }
}

/// Returns the named color for one of the 16 basic colors.
fn basic(color: AnsiColor) -> Color {
    BASIC[usize::from(Ansi256Color::from_ansi(color).index())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_depth_follows_how_many_colors_the_terminal_reports() {
        assert_eq!(ColorDepth::from_count(u16::MAX), ColorDepth::TrueColor);
        assert_eq!(ColorDepth::from_count(256), ColorDepth::Ansi256);
        assert_eq!(ColorDepth::from_count(8), ColorDepth::Ansi16);
        let detected = ColorDepth::detect();
        assert_eq!(
            detected,
            ColorDepth::from_count(crossterm::style::available_color_count())
        );
    }

    #[test]
    fn colors_become_the_nearest_the_terminal_can_show() {
        let red = Color::Rgb(255, 0, 0);
        assert_eq!(ColorDepth::TrueColor.fit(red), red);
        assert_eq!(ColorDepth::Ansi256.fit(red), Color::Indexed(196));
        assert_eq!(
            ColorDepth::Ansi16.fit(Color::Rgb(250, 250, 250)),
            Color::White
        );
        assert_eq!(ColorDepth::Ansi16.fit(Color::Rgb(5, 5, 5)), Color::Black);
    }

    #[test]
    fn colors_that_are_not_24_bit_are_left_alone() {
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16] {
            assert_eq!(depth.fit(Color::Reset), Color::Reset);
            assert_eq!(depth.fit(Color::Indexed(42)), Color::Indexed(42));
        }
    }

    #[test]
    fn every_cell_of_a_drawn_screen_is_fitted() {
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 2, 1));
        buffer.set_style(buffer.area, STATUS);
        ColorDepth::Ansi16.fit_buffer(&mut buffer);
        for cell in &buffer.content {
            assert!(!matches!(cell.fg, Color::Rgb(..)), "{cell:?}");
            assert!(!matches!(cell.bg, Color::Rgb(..)), "{cell:?}");
        }
    }
}
