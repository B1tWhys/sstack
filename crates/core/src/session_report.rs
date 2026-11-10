//! What a Session tells the interface showing it, shared by every interface.
//!
//! An interface hands core a [`ReportSender`] for each Session it opens, and core reports through
//! it everything the person should see: Turns starting and ending, what the model does during
//! them, and questions the person has to answer before a Turn can go on.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::agent::{AgentEvent, TurnEnd};
use crate::question::{Asker, Question, Response};
use crate::session::Entry;
use crate::tools::{Approver, Verdict};

/// Something that happened in a Session, which the interface showing it should react to.
#[derive(Debug, Clone)]
pub enum SessionReport {
    /// The agent picked up a Session that already has these entries, oldest first, such as after
    /// a Rebuild. Comes before any Turn, and holds nothing for a new Session.
    Resumed(Vec<Entry>),
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

/// The Approver that lets the person decide in the interface whether each shell command runs,
/// while the Turn waits.
///
/// A command is denied if the interface drops the question unanswered, such as when the Turn is
/// cancelled or the interface has gone. The Running Harness can also answer
/// [`Verdict::Defer`], so that a Rebuild need not wait for the person.
pub struct ReportingApprover {
    /// Where questions are sent for the interface to show.
    reports: ReportSender,
}

impl ReportingApprover {
    /// Creates an approver that asks through the interface receiving `reports`.
    #[must_use]
    pub fn new(reports: ReportSender) -> Self {
        Self { reports }
    }
}

#[async_trait]
impl Approver for ReportingApprover {
    async fn approve(&self, command: &str, note: Option<&str>) -> Verdict {
        let (reply, answer) = Reply::new();
        let request = ApprovalRequest {
            command: command.to_owned(),
            note: note.map(str::to_owned),
            reply,
        };
        self.reports.send(SessionReport::Approval(request));
        answer.await.unwrap_or(Verdict::Deny)
    }
}

/// The Asker that puts the model's Questions to the person in the interface, while the Turn
/// waits.
///
/// The person gives no answer if the interface drops the Questions unanswered, such as when the
/// Turn is cancelled or the interface has gone.
pub struct ReportingAsker {
    /// Where Questions are sent for the interface to show.
    reports: ReportSender,
}

impl ReportingAsker {
    /// Creates an asker that asks through the interface receiving `reports`.
    #[must_use]
    pub fn new(reports: ReportSender) -> Self {
        Self { reports }
    }
}

#[async_trait]
impl Asker for ReportingAsker {
    async fn ask(&self, questions: Vec<Question>) -> Response {
        let (reply, answer) = Reply::new();
        let request = QuestionRequest { questions, reply };
        self.reports.send(SessionReport::Question(request));
        answer.await.unwrap_or(Response::Declined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::Answer;
    use tokio::sync::mpsc;

    /// Returns a sender whose reports arrive on the returned receiver, or go nowhere once it is
    /// dropped.
    fn channel() -> (ReportSender, mpsc::UnboundedReceiver<SessionReport>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let reports = ReportSender::new(move |report| {
            let _ = sender.send(report);
        });
        (reports, receiver)
    }

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

    #[tokio::test]
    async fn the_answer_given_in_the_interface_decides() {
        let (reports, mut received) = channel();
        let approver = ReportingApprover::new(reports);
        let asking = tokio::spawn(async move { approver.approve("ls", Some("why")).await });

        let Some(SessionReport::Approval(request)) = received.recv().await else {
            panic!("expected an approval request");
        };
        assert_eq!(request.command, "ls");
        assert_eq!(request.note.as_deref(), Some("why"));
        request.reply.send(Verdict::Allow);

        assert_eq!(asking.await.unwrap(), Verdict::Allow);
    }

    fn question() -> Question {
        Question {
            header: "Name".into(),
            text: "What name?".into(),
            choices: vec![],
            recommended: None,
        }
    }

    #[tokio::test]
    async fn the_answers_given_in_the_interface_go_back_to_the_model() {
        let (reports, mut received) = channel();
        let asker = ReportingAsker::new(reports);
        let asking = tokio::spawn(async move { asker.ask(vec![question()]).await });

        let Some(SessionReport::Question(request)) = received.recv().await else {
            panic!("expected questions");
        };
        assert_eq!(request.questions, [question()]);
        request
            .reply
            .send(Response::Answered(vec![Answer::Own("sstack".into())]));

        let answers = asking.await.unwrap();
        assert_eq!(
            answers,
            Response::Answered(vec![Answer::Own("sstack".into())])
        );
    }

    #[tokio::test]
    async fn nobody_to_answer_gives_no_answers() {
        let (reports, received) = channel();
        drop(received);
        let asker = ReportingAsker::new(reports);
        assert_eq!(asker.ask(vec![question()]).await, Response::Declined);
    }

    #[tokio::test]
    async fn nobody_to_ask_denies() {
        let (reports, received) = channel();
        drop(received);
        let approver = ReportingApprover::new(reports);
        assert_eq!(approver.approve("ls", None).await, Verdict::Deny);
    }
}
