//! The messages that drive the interactive harness.

use crossterm::event::KeyEvent;
use sstack::session_report::{ReportSender, SessionReport};
use sstack::vim::VimEvent;
use tokio::sync::mpsc;

/// The channel any task uses to tell the [`App`](crate::tui::app::App) that something happened.
/// Sending never blocks, so it is safe from inside a Turn's event callback.
pub type EventSender = mpsc::UnboundedSender<Event>;

/// The [`App`](crate::tui::app::App)'s end of the event channel, which delivers what happened in
/// the order it was sent.
pub type EventReceiver = mpsc::UnboundedReceiver<Event>;

/// Something that happened, which the [`App`](crate::tui::app::App) passes on to its components.
///
/// Key presses go to the first component that wants them. Every other event goes to every
/// component of one Session: the one it came from if it is an [`Event::Session`], or the one on
/// screen.
#[derive(Debug, Clone)]
pub enum Event {
    /// The person pressed a key.
    Key(KeyEvent),
    /// The person pasted text into the terminal in one piece. It may span several lines.
    Paste(String),
    /// The terminal changed size. Components need not react, because the
    /// [`App`](crate::tui::app::App) redraws anyway.
    Resize,
    /// The person turned the mouse wheel or swiped on a trackpad, wherever the pointer was.
    /// Each notch or swipe step arrives as its own event.
    Scroll(Scroll),
    /// The person clicked the main mouse button at this screen position.
    Click {
        /// Columns from the left edge of the screen.
        column: u16,
        /// Rows from the top edge of the screen.
        row: u16,
    },
    /// The Session with this id did what the inner event says. The [`App`](crate::tui::app::App)
    /// unwraps these, so components never see one.
    Session(String, Box<Event>),
    /// Core reported something the person should see: what a Session's Turns did, a question
    /// about a shell command, the model's Questions, a notice, or the end of the Rebuild's build.
    Report(SessionReport),
    /// The nvim behind the Session's input reported something.
    Vim(VimEvent),
    /// Putting a Session away in the background finished.
    Archived {
        /// The id of the Session that was being put away.
        id: String,
        /// Nothing if the Session is gone, or why it could not be put away.
        outcome: Result<(), String>,
    },
}

/// The channel a Session's agent uses to report to the [`App`](crate::tui::app::App), which keeps
/// it apart from other Sessions' reports.
#[derive(Debug, Clone)]
pub struct SessionSender {
    /// The id of the Session being reported on.
    session: String,
    /// Where reports go.
    events: EventSender,
}

impl SessionSender {
    /// Creates a sender that reports on the Session with id `session` to `events`.
    pub fn new(session: &str, events: EventSender) -> Self {
        Self {
            session: session.to_owned(),
            events,
        }
    }

    /// Reports `event`, which goes nowhere if the [`App`](crate::tui::app::App) has gone. Never
    /// blocks, so it is safe from inside a Turn's event callback.
    pub fn send(&self, event: Event) {
        let _ = self
            .events
            .send(Event::Session(self.session.clone(), Box::new(event)));
    }

    /// Returns a way for core to report on the Session, whose reports reach the
    /// [`App`](crate::tui::app::App) as [`Event::Report`]s from this Session.
    pub fn reporter(&self) -> ReportSender {
        let sender = self.clone();
        ReportSender::new(move |report| sender.send(Event::Report(report)))
    }
}

/// Which way the person scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Towards older output.
    Up,
    /// Towards newer output.
    Down,
}

/// Something a component asks the [`App`](crate::tui::app::App) to do, because it reaches beyond
/// that component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The person entered a line, which may be a Human Command or input for the model.
    Submit(String),
    /// The person asked to stop: cancels the running Turn, or quits if there is none.
    Interrupt,
    /// The person answered a question about a shell command.
    ApprovalAnswered,
    /// The person answered the model's Questions, or declined to, which the transcript records
    /// in these words.
    QuestionsAnswered(String),
    /// The person asked to see the Session below this one in the sidebar.
    NextSession,
    /// The person asked to see the Session above this one in the sidebar.
    PreviousSession,
    /// The person asked to see the Session that has been waiting for them the longest.
    LongestWaiting,
    /// The person asked to put the Session on screen away.
    Archive,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_arrive_from_their_session() {
        let (sender, mut events) = mpsc::unbounded_channel();
        let reporter = SessionSender::new("7", sender).reporter();
        reporter.send(SessionReport::TurnContinued);

        let Ok(Event::Session(session, inner)) = events.try_recv() else {
            panic!("expected an event from a Session");
        };
        assert_eq!(session, "7");
        assert!(matches!(
            *inner,
            Event::Report(SessionReport::TurnContinued)
        ));
    }
}
