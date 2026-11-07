//! Keeping a Session on screen, and replacing it with a new one when the person asks.

use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::Backend;
use sstack_core::workspace;

use super::app::{App, Exit};
use super::event::EventReceiver;
use crate::AnyError;

/// A Session on screen.
pub struct Shown {
    /// Shows the Session and takes the person's input for it.
    pub app: App,
    /// Where the Session is stored, which identifies it across a relaunch.
    pub session: PathBuf,
}

/// Shows one Session at a time, moving to a new one whenever the person asks.
///
/// `open_new` starts a new Session and an App to show it. Use a Screen for every App, so that
/// `/new` works.
pub struct Screen<F> {
    /// The Session on screen now.
    pub shown: Shown,
    /// Starts a new Session, failing with the reason if it cannot.
    pub open_new: F,
}

impl<F: FnMut() -> Result<Shown, AnyError>> Screen<F> {
    /// Runs on `terminal` until the person quits or asks for a Rebuild, never returning
    /// [`Exit::NewSession`].
    ///
    /// # Errors
    /// Returns an error if drawing to the terminal fails.
    pub async fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        events: &mut EventReceiver,
    ) -> Result<Exit, B::Error> {
        loop {
            let exit = self.shown.app.run(terminal, events).await?;
            if exit != Exit::NewSession {
                return Ok(exit);
            }
            self.start_new();
        }
    }

    /// Moves to a new Session, saying how to get back to the one left behind. If no new Session
    /// can be opened, says why and stays.
    fn start_new(&mut self) {
        match (self.open_new)() {
            Ok(shown) => {
                let left = workspace::session_id(&self.shown.session);
                self.shown = shown;
                let notice = format!("started a new Session; resume the last with --resume {left}");
                self.shown.app.notice(notice);
            }
            Err(error) => {
                let notice = format!("could not start a new Session: {error}");
                self.shown.app.notice(notice);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::approver::EventApprover;
    use crate::tui::event::{Event, EventSender};
    use crate::tui::test_support::{agent, key, rows, session_path};
    use crate::tui::theme::ColorDepth;
    use crate::tui::worker::AgentHandle;
    use crossterm::event::KeyCode;
    use ratatui::backend::TestBackend;
    use sstack_core::provider::scripted::ScriptedProvider;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// Returns a Session in `directory`, shown with the id `id`.
    fn shown(directory: &Path, id: &str, events: &EventSender) -> Shown {
        let approver = Arc::new(EventApprover::new(events.clone()));
        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let agent = AgentHandle::spawn(agent(directory, provider, approver), events.clone());
        Shown {
            app: App::new(agent, id.into(), ColorDepth::TrueColor),
            session: session_path(directory),
        }
    }

    /// Queues each line as typed keys, each followed by Enter.
    fn type_lines(events: &EventSender, lines: &[&str]) {
        for line in lines {
            for character in line.chars() {
                events
                    .send(Event::Key(key(KeyCode::Char(character))))
                    .unwrap();
            }
            events.send(Event::Key(key(KeyCode::Enter))).unwrap();
        }
    }

    #[tokio::test]
    async fn asking_for_a_new_session_moves_to_it_and_says_how_to_get_back() {
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (sender, mut events) = mpsc::unbounded_channel();
        let mut screen = Screen {
            shown: shown(first.path(), "old", &sender),
            open_new: || Ok(shown(second.path(), "new", &sender)),
        };
        type_lines(&sender, &["/n", "/quit"]);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();

        let exit = screen.run(&mut terminal, &mut events).await;

        assert_eq!(exit, Ok(Exit::Quit));
        assert_eq!(screen.shown.session, session_path(second.path()));
        let drawn = rows(&terminal).join("\n");
        assert!(drawn.contains("session new"), "{drawn}");
        assert!(
            drawn.contains("resume the last with --resume session"),
            "{drawn}"
        );
    }

    #[tokio::test]
    async fn if_no_new_session_can_open_the_current_one_stays() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, mut events) = mpsc::unbounded_channel();
        let mut screen = Screen {
            shown: shown(dir.path(), "old", &sender),
            open_new: || Err("no keychain".into()),
        };
        type_lines(&sender, &["/new", "/rebuild"]);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();

        let exit = screen.run(&mut terminal, &mut events).await;

        assert_eq!(exit, Ok(Exit::Rebuild));
        assert_eq!(screen.shown.session, session_path(dir.path()));
        let drawn = rows(&terminal).join("\n");
        assert!(drawn.contains("session old"), "{drawn}");
        assert!(
            drawn.contains("could not start a new Session: no keychain"),
            "{drawn}"
        );
    }
}
