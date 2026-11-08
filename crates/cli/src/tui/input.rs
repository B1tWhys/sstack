//! Events for the [`App`](crate::tui::app::App), made from what the terminal reports.

use std::io;

use crossterm::event::{
    Event as TerminalEvent, EventStream, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use futures::{Stream, StreamExt};

use super::event::{Event, EventSender, Scroll};

/// Starts passing terminal input to `events`, until the terminal or the
/// [`App`](crate::tui::app::App) goes away.
pub fn spawn(events: EventSender) {
    tokio::spawn(forward(EventStream::new(), events));
}

/// Passes each useful piece of terminal input to `events`, stopping at the first read error.
async fn forward(
    mut input: impl Stream<Item = io::Result<TerminalEvent>> + Unpin,
    events: EventSender,
) {
    while let Some(Ok(reported)) = input.next().await {
        if let Some(event) = from_terminal(reported)
            && events.send(event).is_err()
        {
            break;
        }
    }
}

/// Returns the event for something the terminal reported, or nothing if the harness has no use
/// for it.
///
/// Only key presses count, because some terminals also report releases and repeats. Of the
/// mouse, only vertical scrolling and left clicks count.
fn from_terminal(event: TerminalEvent) -> Option<Event> {
    match event {
        TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => Some(Event::Key(key)),
        TerminalEvent::Paste(text) => Some(Event::Paste(text)),
        TerminalEvent::Resize(..) => Some(Event::Resize),
        TerminalEvent::Mouse(mouse) => from_mouse(mouse),
        _ => None,
    }
}

/// Returns the event for a mouse action, or nothing if the harness has no use for it.
fn from_mouse(mouse: MouseEvent) -> Option<Event> {
    match mouse.kind {
        MouseEventKind::ScrollUp => Some(Event::Scroll(Scroll::Up)),
        MouseEventKind::ScrollDown => Some(Event::Scroll(Scroll::Down)),
        MouseEventKind::Down(MouseButton::Left) => Some(Event::Click {
            column: mouse.column,
            row: mouse.row,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers, MouseEvent};
    use tokio::sync::mpsc;

    fn key(kind: KeyEventKind) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        })
    }

    /// Returns a mouse action of `kind` somewhere on the screen.
    fn mouse(kind: MouseEventKind) -> TerminalEvent {
        TerminalEvent::Mouse(MouseEvent {
            kind,
            column: 3,
            row: 4,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[tokio::test]
    async fn useful_input_is_forwarded_until_an_error() {
        let input = futures::stream::iter([
            Ok(key(KeyEventKind::Press)),
            Ok(key(KeyEventKind::Release)),
            Ok(TerminalEvent::FocusGained),
            Ok(TerminalEvent::Paste("text".into())),
            Ok(TerminalEvent::Resize(80, 24)),
            Ok(mouse(MouseEventKind::ScrollUp)),
            Ok(mouse(MouseEventKind::Moved)),
            Ok(mouse(MouseEventKind::ScrollLeft)),
            Ok(mouse(MouseEventKind::ScrollDown)),
            Err(io::Error::other("gone")),
            Ok(TerminalEvent::Paste("never seen".into())),
        ]);
        let (sender, mut events) = mpsc::unbounded_channel();

        forward(input, sender).await;

        let mut forwarded = Vec::new();
        while let Ok(event) = events.try_recv() {
            forwarded.push(format!("{event:?}"));
        }
        assert_eq!(forwarded.len(), 5, "{forwarded:?}");
        assert!(forwarded[0].starts_with("Key("));
        assert_eq!(forwarded[1], r#"Paste("text")"#);
        assert_eq!(forwarded[2..], ["Resize", "Scroll(Up)", "Scroll(Down)"]);
    }

    #[tokio::test]
    async fn forwarding_stops_once_the_app_is_gone() {
        let (sender, events) = mpsc::unbounded_channel();
        drop(events);
        let endless = futures::stream::repeat_with(|| Ok(key(KeyEventKind::Press)));
        forward(endless, sender).await;
    }
}
