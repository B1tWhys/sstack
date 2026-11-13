//! What a Session tells the interface showing it, shared by every interface.
//!
//! An interface hands core a [`ReportSender`] for each Session it opens, and core reports through
//! it everything the person should see: Turns starting and ending, what the model does during
//! them, and questions the person has to answer before a Turn can go on.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use super::question::{Question, Response};
use super::turn::{AgentEvent, ToolCallSummary, TurnEnd};

/// Something that happened in a Session, which the interface showing it should react to.
#[derive(Debug, Clone)]
pub enum SessionReport {
    /// The agent picked up a Session that already holds this, oldest first, such as after a
    /// Rebuild. Comes before any Turn, and holds nothing for a new Session.
    Resumed(Vec<Replayed>),
    /// The agent started a Turn on this input from the person.
    TurnStarted(String),
    /// The agent carried on a Turn that had paused.
    TurnContinued,
    /// Something happened during the running Turn.
    Agent(AgentEvent),
    /// The running Turn ended, either normally or with the error that stopped it.
    TurnEnded(Result<TurnEnd, String>),
    /// The model wants to run a shell command, and the Turn is waiting for the person to decide,
    /// unless a Rebuild puts the question off.
    Approval(ApprovalRequest),
    /// The model put Questions to the person, and the Turn is waiting for their answers, unless
    /// a Rebuild puts the Questions off.
    Question(QuestionRequest),
    /// The Rebuild's build finished, with the new binary or what went wrong.
    BuildFinished(Result<PathBuf, String>),
    /// The Running Harness has something to tell the person, such as a mistyped Human Command.
    Notice(String),
    /// The attempt to give the Session its Session Name from the person's first input ended, with
    /// the name it gave, or nothing if it gave none, as when the model failed or the person named
    /// the Session first.
    Named(Option<String>),
}

/// Something a resumed Session already holds, for the interface to show as it showed it live.
///
/// It is made from the Session's log, but leaves out what only the model needs, so that the log
/// can keep its shape for older Sessions while what interfaces read changes freely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replayed {
    /// The person said this.
    Prompt(String),
    /// The model replied with this text, which is never empty.
    Reply(String),
    /// A tool call started.
    ToolStarted {
        /// The id that links the call to how it finished.
        id: String,
        /// The call, as the person watching it run saw it.
        call: ToolCallSummary,
    },
    /// A tool call finished.
    ToolFinished {
        /// The id of the call that finished.
        id: String,
        /// Whether the call failed.
        is_error: bool,
    },
    /// A Turn ended this way, or a response ended this way within one. Most ends need no saying.
    TurnEnded(TurnEnd),
}

/// Where one Session's reports go, chosen by the interface showing it.
///
/// Clones report to the same place. Sending never blocks, so it is safe from inside a Turn's
/// event callback.
#[derive(Clone)]
pub struct ReportSender(Arc<dyn Fn(SessionReport) + Send + Sync>);

impl ReportSender {
    /// Creates a sender that hands each report to `deliver`, which must not block.
    pub fn new(deliver: impl Fn(SessionReport) + Send + Sync + 'static) -> Self {
        Self(Arc::new(deliver))
    }

    /// Reports `report`.
    pub fn send(&self, report: SessionReport) {
        (self.0)(report);
    }
}

/// A question asking the person whether a command may run on the host.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// The command, exactly as it would run.
    pub command: String,
    /// Why the person is being asked, if the asker says.
    pub note: Option<String>,
    /// Where the person's decision goes. Dropping it unanswered denies the command. Only the
    /// first answer counts, so one sent after a Rebuild put the question off does nothing.
    pub reply: Reply<Verdict>,
}

/// The model's Questions to the person, waiting for answers.
#[derive(Debug, Clone)]
pub struct QuestionRequest {
    /// The Questions, in the order the person answers them.
    pub questions: Vec<Question>,
    /// Where the answers go, or word that the person would rather answer in the conversation.
    /// Dropping it unanswered counts as declining. Only the first response counts, so answers
    /// sent after a Rebuild put the Questions off do nothing.
    pub reply: Reply<Response>,
}

/// A reply to a question that another task is waiting on.
///
/// Clones share one answer, and only the first answer given counts. Once every clone is dropped
/// unanswered, the waiting task sees that no answer will come.
#[derive(Debug, Clone)]
pub struct Reply<T>(Arc<Mutex<Option<oneshot::Sender<T>>>>);

impl<T> Reply<T> {
    /// Creates a question, returning the way to answer it and the way to wait for the answer.
    #[must_use]
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

/// What became of a question whether the model may run a command on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The command may run.
    Allow,
    /// The command may not run, and the model is told so.
    Deny,
    /// Nobody decided, because a Rebuild put the question off. The call that asked runs again,
    /// asking again, when its Turn carries on.
    Defer,
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
