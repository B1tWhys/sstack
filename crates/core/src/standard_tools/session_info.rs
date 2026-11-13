use std::sync::PoisonError;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::connection::ToolSpec;
use crate::tools::{SharedSessionInfo, Tool, ToolError};

/// The tool that tells the model about the Session it is working in.
pub(super) struct ShowSessionInfo {
    /// What the tool tells the model, as of the moment it is called.
    info: SharedSessionInfo,
}

impl ShowSessionInfo {
    /// Creates the tool, telling the model whatever `info` holds when it is called.
    pub(super) fn new(info: SharedSessionInfo) -> Self {
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
    use std::sync::{Arc, Mutex};

    use crate::connection::Effort;
    use crate::tools::SessionInfo;

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
