//! Hints that appear while the pointer rests on part of the window, all with the same look.

use std::time::Duration;

use iced::Element;
use iced::widget::{container, text, tooltip};

/// How long the pointer must rest before a hint appears, so that moving across the window does
/// not flash hints along the way.
const DELAY: Duration = Duration::from_millis(500);

/// The widest a hint grows, in logical pixels, before its text wraps onto another line.
const MAX_WIDTH: f32 = 320.0;

/// The room around a hint's text, and between a hint and what it describes, in logical pixels.
const PADDING: f32 = 6.0;

/// Shows `tip` just below `content` once the pointer rests on it, in the one look every hint in
/// the GUI shares. Long text wraps, and the hint stays inside the window.
pub fn hint<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    tip: impl Into<String>,
) -> Element<'a, Message> {
    let tip = container(text(tip.into()))
        .max_width(MAX_WIDTH)
        .padding(PADDING)
        .style(container::bordered_box);
    tooltip(content, tip, tooltip::Position::Bottom)
        .gap(PADDING)
        .delay(DELAY)
        .snap_within_viewport(true)
        .into()
}
