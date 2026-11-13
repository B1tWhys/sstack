//! What happens during a Turn and how it ends, as the person watching it is told.

/// Something that happened during a Turn, reported as it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// A piece of the model's reply.
    Text(String),
    /// A piece of the model's summarized thinking.
    Reasoning(String),
    /// A tool call is about to run.
    ToolStarted(ToolCallSummary),
    /// A tool call finished.
    ToolFinished {
        /// The name of the tool that ran.
        name: String,
        /// Whether the call failed.
        is_error: bool,
    },
    /// The person's Queued Prompts joined into this text, which the Turn has just taken and the
    /// model reads in its next Step.
    Admitted(String),
    /// The latest request used most of the context window, and requests fail once it is full.
    ContextNearLimit {
        /// Tokens in the most recent request.
        used: u64,
        /// The model's context window, in tokens.
        limit: u64,
    },
}

/// A description of a tool call for the person watching it run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallSummary {
    /// The name of the tool being called.
    pub name: String,
    /// The call's input, as JSON text.
    pub input: String,
}

/// How a Turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    /// The model finished what it was doing.
    Finished,
    /// The response reached the output token limit.
    Truncated,
    /// The model declined the request.
    Refused,
    /// The person cancelled the Turn.
    Cancelled,
    /// The Turn stopped between Steps when asked to, and
    /// [`Agent::continue_turn`](crate::agent::Agent::continue_turn) picks it up.
    Paused,
    /// The Turn stopped partway through a Step because a Rebuild put off what it asked the person,
    /// and [`Agent::continue_turn`](crate::agent::Agent::continue_turn) picks it up by asking the
    /// same thing again. Nothing about it needs telling the person, since the question comes back
    /// as it was.
    PutOff,
}
