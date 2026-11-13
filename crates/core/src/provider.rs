//! The boundary between the harness and the services that run models.

pub mod anthropic;
pub mod deepseek;
pub mod genai_chat;
pub mod openai;
pub mod scripted;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::{AssistantEntry, Entry};

/// How much effort a model should spend on a response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// The least effort, for simple work.
    Low,
    /// A balance between thoroughness and cost.
    Medium,
    /// Thorough effort, and the default.
    #[default]
    High,
    /// Extra thoroughness, for hard work.
    Xhigh,
    /// As much effort as the model can spend.
    Max,
}

/// A tool definition offered to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// The name the model calls the tool by.
    pub name: String,
    /// What the tool does, written for the model.
    pub description: String,
    /// The JSON Schema of the tool's input.
    pub input_schema: Value,
}

/// Everything a [`Provider`] needs to produce one response.
#[derive(Debug, Clone, Copy)]
pub struct ModelRequest<'a> {
    /// The identifier of the model to use.
    pub model: &'a str,
    /// The system prompt.
    pub system: &'a str,
    /// The Session so far, oldest first.
    pub entries: &'a [Entry],
    /// The tools the model may call.
    pub tools: &'a [ToolSpec],
}

/// A piece of a response, reported while it is being generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A piece of the visible reply.
    Text(String),
    /// A piece of the model's summarized thinking.
    Reasoning(String),
}

/// A failed model request, described for the person.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(pub String);

/// A service able to produce model responses.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Streams one model response, reporting progress through `on_event`.
    ///
    /// # Errors
    /// Returns an error if the request fails or the response cannot be understood.
    async fn respond(
        &self,
        request: ModelRequest<'_>,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<AssistantEntry, ProviderError>;
}
