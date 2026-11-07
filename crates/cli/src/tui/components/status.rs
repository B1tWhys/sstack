//! The line that tells the person where they are and what they can do.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::tui::component::Component;
use crate::tui::event::Event;
use crate::tui::theme;

/// Tells the person which Session they are in, whether the agent is working, and how to stop.
#[derive(Debug)]
pub struct Status {
    /// The id of the Session, which `--resume` accepts.
    session: String,
    /// Whether a Turn is running.
    working: bool,
}

impl Status {
    /// Creates a status line for the Session with this id.
    pub fn new(session: String) -> Self {
        Self {
            session,
            working: false,
        }
    }

    /// Returns what the status line says.
    fn text(&self) -> String {
        let (state, stop) = if self.working {
            ("working", "Ctrl-C cancel")
        } else {
            ("ready", "Ctrl-C quit")
        };
        format!(
            "session {} · {state} · {stop} · PgUp/PgDn scroll · /new /rebuild /quit",
            self.session
        )
    }
}

impl Component for Status {
    fn update(&mut self, event: &Event) {
        match event {
            Event::TurnStarted(_) => self.working = true,
            Event::TurnEnded(_) => self.working = false,
            _ => {}
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Line::from(self.text()).style(theme::STATUS), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::screen;
    use sstack_core::agent::TurnEnd;

    #[test]
    fn shows_the_session_and_whether_a_turn_is_running() {
        let mut status = Status::new("42".into());
        assert!(
            status
                .text()
                .starts_with("session 42 · ready · Ctrl-C quit")
        );

        status.update(&Event::TurnStarted("go".into()));
        assert!(status.text().contains("working · Ctrl-C cancel"));

        status.update(&Event::Resize);
        status.update(&Event::TurnEnded(Ok(TurnEnd::Finished)));
        let shown = screen(30, 1, |frame| status.render(frame, frame.area()));
        assert_eq!(shown, ["session 42 · ready · Ctrl-C qu"]);
    }
}
