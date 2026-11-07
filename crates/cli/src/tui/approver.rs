//! Asking the person about shell commands from inside a running Turn.

use async_trait::async_trait;
use sstack_core::tools::Approver;

use super::event::{ApprovalRequest, Event, EventSender, Reply};

/// Lets the person decide on screen whether each shell command runs, while the Turn waits.
///
/// A command is denied if the interactive harness has gone or drops the question unanswered,
/// such as when the Turn is cancelled.
pub struct EventApprover {
    /// Where questions are sent for the App to show.
    events: EventSender,
}

impl EventApprover {
    /// Creates an approver that asks through the App listening on `events`.
    pub fn new(events: EventSender) -> Self {
        Self { events }
    }
}

#[async_trait]
impl Approver for EventApprover {
    async fn approve(&self, command: &str) -> bool {
        let (reply, answer) = Reply::new();
        let request = ApprovalRequest {
            command: command.to_owned(),
            reply,
        };
        let _ = self.events.send(Event::Approval(request));
        answer.await.unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn the_answer_given_on_screen_decides() {
        let (sender, mut events) = mpsc::unbounded_channel();
        let approver = EventApprover::new(sender);
        let asking = tokio::spawn(async move { approver.approve("ls").await });

        let Some(Event::Approval(request)) = events.recv().await else {
            panic!("expected an approval request");
        };
        assert_eq!(request.command, "ls");
        request.reply.send(true);

        assert!(asking.await.unwrap());
    }

    #[tokio::test]
    async fn nobody_to_ask_denies() {
        let (sender, events) = mpsc::unbounded_channel();
        drop(events);
        assert!(!EventApprover::new(sender).approve("ls").await);
    }
}
