//! The boundary between the harness and the services that run models.

pub mod scripted;

use async_trait::async_trait;
use serde_json::Value;

use crate::session::{AssistantEntry, Entry};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelRequest<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub entries: &'a [Entry],
    pub tools: &'a [ToolSpec],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Text(String),
    Reasoning(String),
}

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
