//! The messages that drive the interactive harness.

use std::sync::{Arc, Mutex};

use crossterm::event::KeyEvent;
use sstack_core::agent::{AgentEvent, TurnEnd};
use sstack_core::session::Entry;
use tokio::sync::{mpsc, oneshot};

/// Lets any task tell the App that something happened. Sending never blocks, so it is safe from
/// inside a Turn's event callback.
pub type EventSender = mpsc::UnboundedSender<Event>;

/// Delivers what happened to the App, in the order it was sent.
pub type EventReceiver = mpsc::UnboundedReceiver<Event>;

/// Something that happened, which the App passes on to its components.
///
/// Key presses go to the first component that wants them. Every other event goes to every
/// component.
#[derive(Debug, Clone)]
pub enum Event {
    /// The person pressed a key.
    Key(KeyEvent),
    /// The person pasted text into the terminal in one piece. It may span several lines.
    Paste(String),
    /// The terminal changed size. Components need not react, because the App redraws anyway.
    Resize,
    /// The agent picked up a Session that already has these entries, oldest first, such as after
    /// a Rebuild. Comes before any Turn, and holds nothing for a new Session.
    Resumed(Vec<Entry>),
    /// The agent started a Turn on this input from the person.
    TurnStarted(String),
    /// Something happened during the running Turn.
    Agent(AgentEvent),
    /// The running Turn ended, either normally or with the error that stopped it.
    TurnEnded(Result<TurnEnd, String>),
    /// The model wants to run a shell command, and the Turn is waiting for the person to decide.
    Approval(ApprovalRequest),
    /// The harness has something to tell the person, such as a mistyped Human Command.
    Notice(String),
}

/// Something a component asks the App to do, because it reaches beyond that component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The person entered a line, which may be a Human Command or input for the model.
    Submit(String),
    /// The person asked to stop: cancels the running Turn, or quits if there is none.
    Interrupt,
}

/// Asks the person whether a shell command may run.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// The command, exactly as it would run.
    pub command: String,
    /// Where the person's decision goes. Dropping it unanswered denies the command.
    pub reply: Reply<bool>,
}

/// Answers a question that another task is waiting on.
///
/// Clones share one answer, and only the first answer given counts. Once every clone is dropped
/// unanswered, the waiting task sees that no answer will come.
#[derive(Debug, Clone)]
pub struct Reply<T>(Arc<Mutex<Option<oneshot::Sender<T>>>>);

impl<T> Reply<T> {
    /// Creates a question, returning the way to answer it and the way to wait for the answer.
    pub fn new() -> (Self, oneshot::Receiver<T>) {
        let (sender, receiver) = oneshot::channel();
        (Self(Arc::new(Mutex::new(Some(sender)))), receiver)
    }

    /// Gives the answer, unless one was already given or nobody is waiting any more.
    pub fn send(&self, answer: T) {
        let sender = self.0.lock().ok().and_then(|mut sender| sender.take());
        if let Some(sender) = sender {
            let _ = sender.send(answer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_the_first_answer_counts() {
        let (reply, answer) = Reply::new();
        let copy = reply.clone();
        copy.send(true);
        reply.send(false);
        assert_eq!(answer.await, Ok(true));
    }

    #[tokio::test]
    async fn dropping_every_copy_unanswered_closes_the_question() {
        let (reply, answer) = Reply::<bool>::new();
        drop(reply.clone());
        drop(reply);
        assert!(answer.await.is_err());
    }

    #[test]
    fn answering_when_nobody_waits_is_harmless() {
        let (reply, answer) = Reply::new();
        drop(answer);
        reply.send(1);
    }
}
