//! What passes between core and the interfaces, in both directions: what a Session reports to the
//! interface showing it, and the person's answers that go back.
//!
//! This module imports nothing else in core, so that the interfaces and core can both depend on
//! it without depending on each other's details.

mod question;
mod report;
mod turn;

pub use question::{Answer, Choice, Question, Response, describe_all};
pub use report::{
    ApprovalRequest, QuestionRequest, Replayed, Reply, ReportSender, SessionReport, Verdict,
};
pub use turn::{AgentEvent, ToolCallSummary, TurnEnd};
