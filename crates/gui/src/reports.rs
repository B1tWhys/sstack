//! The way a Session's reports reach the iced app, which reads them through a [`Subscription`].

use iced::Subscription;
use sstack::protocol::{ReportSender, SessionReport};

use crate::inbox::Inbox;

/// The reports of one Session as they arrive, waiting for the window that shows it to read them.
///
/// Clones read the same reports, and only the first stream taken from them gets any. They can be
/// made before the Session has an id, so that the Session's Approver can ask through them.
#[derive(Clone)]
pub struct Reports {
    /// The reports not yet read.
    inbox: Inbox<SessionReport>,
}

impl Reports {
    /// Creates the reports of one Session, returning them with the sender that core reports
    /// through.
    pub fn new() -> (Self, ReportSender) {
        let (inbox, sender) = Inbox::new();
        let deliver = move |report| {
            let _ = sender.unbounded_send(report);
        };
        (Self { inbox }, ReportSender::new(deliver))
    }

    /// Returns the subscription that delivers each report to the app as it arrives.
    pub fn subscription(&self) -> Subscription<SessionReport> {
        self.inbox.subscription()
    }

    /// Takes the reports as a stream that ends once core can send no more. A stream taken after
    /// the first is empty.
    #[cfg(test)]
    pub fn stream(&self) -> impl iced::futures::Stream<Item = SessionReport> + use<> {
        self.inbox.stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::StreamExt;
    use iced::futures::executor::block_on;

    #[test]
    fn reports_arrive_in_order_on_the_first_stream_taken() {
        let (reports, sender) = Reports::new();
        sender.send(SessionReport::TurnContinued);
        sender.send(SessionReport::Notice("hi".into()));
        drop(sender);

        let read: Vec<_> = block_on(reports.stream().collect());
        assert!(matches!(
            read.as_slice(),
            [SessionReport::TurnContinued, SessionReport::Notice(text)] if text == "hi"
        ));
        let again: Vec<_> = block_on(reports.clone().stream().collect());
        assert!(again.is_empty());
    }

    #[test]
    fn reporting_after_the_window_has_gone_is_harmless() {
        let (reports, sender) = Reports::new();
        drop(reports);
        sender.send(SessionReport::TurnContinued);
    }
}
