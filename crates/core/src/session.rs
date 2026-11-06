//! The durable record of a Session.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// The data a Provider returned for a model response, opaque to everything but that Provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderPayload {
    pub provider: String,
    pub model: String,
    pub data: Value,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantEntry {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop: StopReason,
    pub usage: Usage,
    pub payload: Option<ProviderPayload>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    User {
        text: String,
    },
    Assistant(AssistantEntry),
    ToolResult {
        call_id: String,
        output: String,
        is_error: bool,
    },
    TurnCancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session file error: {0}")]
    Io(#[from] io::Error),
    #[error("session entry is malformed: {0}")]
    Malformed(#[from] serde_json::Error),
}

/// An open, append-only Session log.
#[derive(Debug)]
pub struct SessionLog {
    file: File,
}

impl SessionLog {
    /// Creates a new log at `path`, including any missing parent directories.
    ///
    /// # Errors
    /// Returns an error if the file already exists or cannot be created.
    pub fn create(path: &Path) -> Result<Self, SessionError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)?;
        Ok(Self { file })
    }

    /// Opens an existing log and returns it with its entries.
    ///
    /// Bytes after the last newline are discarded: a crash mid-append leaves a partial line
    /// that was never a recorded entry, and appending after it would corrupt the next one.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or a complete entry is malformed.
    pub fn open(path: &Path) -> Result<(Self, Vec<Entry>), SessionError> {
        let mut file = OpenOptions::new().read(true).append(true).open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let complete = contents.rfind('\n').map_or("", |end| &contents[..=end]);
        file.set_len(complete.len() as u64)?;
        let entries = complete
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Entry>, _>>()?;
        Ok((Self { file }, entries))
    }

    /// Durably appends one entry.
    ///
    /// # Errors
    /// Returns an error if the entry cannot be serialized or written.
    pub fn append(&mut self, entry: &Entry) -> Result<(), SessionError> {
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant_entry() -> Entry {
        Entry::Assistant(AssistantEntry {
            text: "hello".into(),
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                name: "read".into(),
                input: json!({"path": "a.rs"}),
            }],
            stop: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 2,
            },
            payload: Some(ProviderPayload {
                provider: "anthropic".into(),
                model: "claude-opus-5-5".into(),
                data: json!([{"opaque": true}]),
            }),
        })
    }

    #[test]
    fn appended_entries_survive_reopening_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/session.jsonl");
        let user = Entry::User { text: "hi".into() };
        let tool = Entry::ToolResult {
            call_id: "call-1".into(),
            output: "contents\nwith newline".into(),
            is_error: false,
        };

        let mut log = SessionLog::create(&path).unwrap();
        for entry in [&user, &assistant_entry(), &tool, &Entry::TurnCancelled] {
            log.append(entry).unwrap();
        }
        drop(log);

        let (_, entries) = SessionLog::open(&path).unwrap();
        assert_eq!(
            entries,
            vec![user, assistant_entry(), tool, Entry::TurnCancelled]
        );
    }

    #[test]
    fn create_refuses_to_overwrite_an_existing_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        SessionLog::create(&path).unwrap();
        assert!(matches!(
            SessionLog::create(&path),
            Err(SessionError::Io(_))
        ));
    }

    #[test]
    fn a_partial_final_line_is_dropped_and_appending_stays_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let first = Entry::User {
            text: "first".into(),
        };
        let second = Entry::User {
            text: "second".into(),
        };
        let mut log = SessionLog::create(&path).unwrap();
        log.append(&first).unwrap();
        drop(log);
        let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
        raw.write_all(b"{\"type\":\"us").unwrap();
        drop(raw);

        let (mut log, entries) = SessionLog::open(&path).unwrap();
        assert_eq!(entries, vec![first.clone()]);
        log.append(&second).unwrap();
        drop(log);

        let (_, entries) = SessionLog::open(&path).unwrap();
        assert_eq!(entries, vec![first, second]);
    }

    #[test]
    fn a_session_with_only_a_partial_line_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, "{\"type\"").unwrap();
        let (_, entries) = SessionLog::open(&path).unwrap();
        assert_eq!(entries, Vec::new());
    }

    #[test]
    fn a_malformed_complete_line_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, "not json\n").unwrap();
        assert!(matches!(
            SessionLog::open(&path),
            Err(SessionError::Malformed(_))
        ));
    }
}
