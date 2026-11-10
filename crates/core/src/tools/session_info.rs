use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

use super::{Tool, ToolError};
use crate::provider::{Effort, ToolSpec};

/// What the model can learn about the Session it is working in, so that it can record in what it
/// leaves behind, such as a commit, which Session made it and with which model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionInfo {
    /// The id of the Session, which names its log and is what `--resume` takes.
    pub session_id: String,
    /// The identifier of the model the Running Harness requests, such as `claude-opus-4-5`.
    pub model: String,
    /// How much effort the model is asked to spend on each response. Missing where the
    /// Connection sets none, and then left out of what the model sees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

/// What the [`ShowSessionInfo`] tool tells the model, shared with whoever keeps it up to date
/// when the Session moves to another Connection.
pub(super) type SharedInfo = Arc<Mutex<SessionInfo>>;

/// The tool that tells the model about the Session it is working in.
pub(super) struct ShowSessionInfo {
    /// What the tool tells the model, as of the moment it is called.
    info: SharedInfo,
}

impl ShowSessionInfo {
    /// Creates the tool, telling the model whatever `info` holds when it is called.
    pub(super) fn new(info: SharedInfo) -> Self {
        Self { info }
    }
}

#[async_trait]
impl Tool for ShowSessionInfo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "session_info".into(),
            description: "Return facts about this Session as JSON: its id, which a person can use \
                to find the Session's log, the model serving it, and the model's effort if one is \
                set."
                .into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    async fn run(&self, _input: Value) -> Result<String, ToolError> {
        let info = self.info.lock().unwrap_or_else(PoisonError::into_inner);
        serde_json::to_string(&*info).map_err(|error| ToolError::Failed(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns what the tool tells the model when it knows `effort`.
    async fn shown(effort: Option<Effort>) -> Value {
        let tool = ShowSessionInfo::new(Arc::new(Mutex::new(SessionInfo {
            session_id: "0001794212925375".into(),
            model: "claude-opus-4-5".into(),
            effort,
        })));
        let text = tool.run(json!({})).await.unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn returns_the_session_id_model_and_effort() {
        let expected = json!({
            "session_id": "0001794212925375",
            "model": "claude-opus-4-5",
            "effort": "xhigh",
        });
        assert_eq!(shown(Some(Effort::Xhigh)).await, expected);
    }

    #[tokio::test]
    async fn leaves_out_an_effort_that_is_not_set() {
        let expected = json!({"session_id": "0001794212925375", "model": "claude-opus-4-5"});
        assert_eq!(shown(None).await, expected);
    }
}
