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
    /// What other Sessions are doing, when the sidebar is not there to say.
    others: Option<String>,
}

impl Status {
    /// Creates a status line for the Session with this id.
    pub fn new(session: String) -> Self {
        Self {
            session,
            working: false,
            others: None,
        }
    }

    /// Sets what to say about other Sessions, replacing whatever was said before.
    pub fn set_others(&mut self, others: Option<String>) {
        self.others = others;
    }

    /// Returns what the status line says.
    fn text(&self) -> String {
        let (state, stop) = if self.working {
            ("working", "Alt-C cancel")
        } else {
            ("ready", "Alt-C quit")
        };
        let others = self
            .others
            .as_ref()
            .map_or_else(String::new, |others| format!(" | {others}"));
        format!(
            "session {} | {state}{others} | {stop} | Alt-E/Y switch | Alt-G next | \
             PgUp/PgDn scroll | /new /vim /rebuild /quit",
            self.session
        )
    }
}

impl Component for Status {
    fn update(&mut self, event: &Event) {
        match event {
            Event::TurnStarted(_) | Event::TurnContinued => self.working = true,
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
    use crate::tui::test_support::drawn;
    use insta::assert_snapshot;
    use sstack::agent::TurnEnd;

    #[test]
    fn shows_the_session_and_whether_a_turn_is_running() {
        let mut status = Status::new("42".into());
        assert_snapshot!(status.text());

        status.update(&Event::TurnStarted("go".into()));
        assert_snapshot!(status.text());

        status.set_others(Some("1 blocked".into()));
        assert_snapshot!(status.text());
        status.set_others(None);

        status.update(&Event::Resize);
        status.update(&Event::TurnEnded(Ok(TurnEnd::Finished)));
        let shown = drawn(30, 1, |frame| status.render(frame, frame.area()));
        assert_snapshot!(shown.backend());
    }
}
