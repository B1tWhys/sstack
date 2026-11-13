//! The Approver and Asker that put the model's questions to the person through the interface
//! showing the Session, while the Turn waits.

use async_trait::async_trait;

use crate::protocol::{
    ApprovalRequest, Question, QuestionRequest, Reply, ReportSender, Response, SessionReport,
    Verdict,
};
use crate::tools::{Approver, Asker};

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
    use crate::protocol::Answer;
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
