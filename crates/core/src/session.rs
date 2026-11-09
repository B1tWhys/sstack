//! The durable record of a Session.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A request from the model to run a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The identifier linking this call to its result.
    pub id: String,
    /// The name of the tool to run.
    pub name: String,
    /// The tool's input, as JSON.
    pub input: Value,
}

/// The data a [`Provider`](crate::provider::Provider) returned for a model response, opaque to
/// everything but that Provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderPayload {
    /// The name of the [`Provider`](crate::provider::Provider) that produced this data.
    pub provider: String,
    /// The identifier of the model that produced this data.
    pub model: String,
    /// The data itself, whose shape only that Provider understands.
    pub data: Value,
}

/// How many tokens a model response used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens sent to the model, including any served from a cache.
    pub input_tokens: u64,
    /// Tokens the model generated.
    pub output_tokens: u64,
}

/// Why a model stopped generating a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its reply.
    EndTurn,
    /// The model is waiting for tool results.
    ToolUse,
    /// The response reached the output token limit.
    MaxTokens,
    /// The model declined the request.
    Refused,
}

/// One response from the model, in a form any [`Provider`](crate::provider::Provider) can read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantEntry {
    /// The visible text of the response.
    pub text: String,
    /// The tools the model asked to run.
    pub tool_calls: Vec<ToolCall>,
    /// Why the response ended.
    pub stop: StopReason,
    /// How many tokens the response used.
    pub usage: Usage,
    /// The exact data from the Provider, used to replay this response to it.
    pub payload: Option<ProviderPayload>,
}

/// One recorded event in a Session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    /// A message from the person.
    User {
        /// The text of the message.
        text: String,
    },
    /// A response from the model.
    Assistant(AssistantEntry),
    /// The outcome of a tool call.
    ToolResult {
        /// The id of the tool call this answers.
        call_id: String,
        /// The output shown to the model.
        output: String,
        /// Whether the call failed.
        is_error: bool,
    },
    /// The person cancelled the Turn at this point.
    TurnCancelled,
    /// The Turn stopped here between Steps so that the harness could stop, and should carry on
    /// once it runs again.
    TurnParked,
}

/// A failure reading or writing a Session log.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The log file could not be read or written.
    #[error("session file error: {0}")]
    Io(#[from] io::Error),
    /// An entry could not be converted to or from its stored form.
    #[error("session entry is malformed: {0}")]
    Malformed(#[from] serde_json::Error),
    /// Another [`SessionLog`] has the Session open, most likely in another Running Harness, and
    /// two of them appending to one log would interleave their entries. Lasts until that one is
    /// dropped or its process ends.
    #[error("this Session is open elsewhere")]
    OpenElsewhere,
}

/// The durable record of a Session, which survives crashes and can be resumed.
#[derive(Debug)]
pub struct SessionLog {
    /// Where entries are stored.
    file: File,
}

impl SessionLog {
    /// Starts a new log at `path`, creating any missing directories. No other [`SessionLog`] can
    /// open the Session until this one is dropped.
    ///
    /// # Errors
    /// Returns an error if the file already exists or cannot be created, and
    /// [`SessionError::OpenElsewhere`] if another log opened it first.
    pub fn create(path: &Path) -> Result<Self, SessionError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)?;
        Ok(Self {
            file: exclusive(file)?,
        })
    }

    /// Opens an existing log and returns it with its entries.
    ///
    /// A partial entry left by a crash is discarded, because it was never recorded. No other
    /// [`SessionLog`], in this process or any other, can open the Session until this one is
    /// dropped.
    ///
    /// # Errors
    /// Returns [`SessionError::OpenElsewhere`] if another log has the Session open, and another
    /// error if the file cannot be read or a complete entry is malformed.
    pub fn open(path: &Path) -> Result<(Self, Vec<Entry>), SessionError> {
        let file = OpenOptions::new().read(true).append(true).open(path)?;
        let mut file = exclusive(file)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let complete = recorded(&contents);
        file.set_len(complete.len() as u64)?;
        Ok((Self { file }, parse(complete)?))
    }

    /// Durably appends one entry.
    ///
    /// # Errors
    /// Returns an error if the entry cannot be stored.
    pub fn append(&mut self, entry: &Entry) -> Result<(), SessionError> {
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()?;
        Ok(())
    }
}

impl Drop for SessionLog {
    /// Frees the Session at once, even while a child process that is still starting holds a
    /// copy of the file, which would otherwise keep it claimed until that child runs its
    /// program.
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Returns the part of a log's `contents` that was fully recorded, without any partial entry a
/// crash or a write in progress left at the end.
fn recorded(contents: &str) -> &str {
    contents.rfind('\n').map_or("", |end| &contents[..=end])
}

/// Reads the entries of a log whose `recorded` contents end at a complete entry.
///
/// # Errors
/// Returns [`SessionError::Malformed`] if any entry is malformed.
fn parse(recorded: &str) -> Result<Vec<Entry>, SessionError> {
    let entries = recorded
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    Ok(entries)
}

/// Returns everything recorded so far in the Session stored at `path`. Does not disturb the
/// log, so it is safe on a Session in use, but gives no way to add to it: that takes
/// [`SessionLog::open`].
///
/// # Errors
/// Returns an error if the file cannot be read or a complete entry is malformed.
pub fn lock_free_read(path: &Path) -> Result<Vec<Entry>, SessionError> {
    parse(recorded(&fs::read_to_string(path)?))
}

/// Claims the Session stored in `file` for the caller alone, until `file` is closed.
///
/// # Errors
/// Returns [`SessionError::OpenElsewhere`] if another open file has claimed it, in this process
/// or any other, and [`SessionError::Io`] if the claim cannot be made at all.
fn exclusive(file: File) -> Result<File, SessionError> {
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(SessionError::OpenElsewhere),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Returns the first thing the person said in the Session stored at `path`, which tells the
/// Session apart from others. Does not disturb the log, so it is safe on a Session in use.
///
/// Returns nothing for a Session the person has not spoken in yet, or whose log is unreadable
/// past the point reached.
///
/// # Errors
/// Returns an error if the file cannot be opened.
pub fn first_prompt(path: &Path) -> io::Result<Option<String>> {
    let lines = BufReader::new(File::open(path)?).lines();
    Ok(lines
        .map_while(Result::ok)
        .map_while(|line| serde_json::from_str(&line).ok())
        .find_map(|entry| match entry {
            Entry::User { text } => Some(text),
            _ => None,
        }))
}

/// Returns whether the Session stored at `path` stopped between Steps and should carry on. Does
/// not disturb the log, so it is safe on a Session in use.
///
/// # Errors
/// Returns an error if the file cannot be read.
pub fn is_parked(path: &Path) -> io::Result<bool> {
    let contents = fs::read_to_string(path)?;
    let last = contents.lines().next_back().unwrap_or_default();
    Ok(serde_json::from_str::<Entry>(last).is_ok_and(|entry| entry == Entry::TurnParked))
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

    /// A child process that is still starting holds a copy of every open file until it runs
    /// its program, and a clone shares the open file the same way.
    #[test]
    fn a_dropped_log_frees_the_session_even_while_a_copy_of_its_file_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let log = SessionLog::create(&path).unwrap();
        let copy = log.file.try_clone().unwrap();

        drop(log);

        assert!(SessionLog::open(&path).is_ok());
        drop(copy);
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
    fn a_session_open_in_one_place_cannot_be_opened_in_another() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let created = SessionLog::create(&path).unwrap();
        assert!(matches!(
            SessionLog::open(&path),
            Err(SessionError::OpenElsewhere)
        ));
        drop(created);

        let (opened, _) = SessionLog::open(&path).unwrap();
        assert!(matches!(
            SessionLog::open(&path),
            Err(SessionError::OpenElsewhere)
        ));
        assert_eq!(lock_free_read(&path).unwrap(), []);
        assert_eq!(first_prompt(&path).unwrap(), None);
        assert!(!is_parked(&path).unwrap());
        drop(opened);

        assert!(SessionLog::open(&path).is_ok());
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

    #[test]
    fn the_first_prompt_comes_from_the_log_without_changing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut log = SessionLog::create(&path).unwrap();
        assert_eq!(first_prompt(&path).unwrap(), None);

        log.append(&Entry::TurnCancelled).unwrap();
        for text in ["first", "second"] {
            let text = text.to_owned();
            log.append(&Entry::User { text }).unwrap();
        }
        let before = fs::read(&path).unwrap();

        assert_eq!(first_prompt(&path).unwrap().as_deref(), Some("first"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_log_that_cannot_be_understood_has_no_first_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        assert!(first_prompt(&path).is_err());
        fs::write(&path, "not json\n").unwrap();
        assert_eq!(first_prompt(&path).unwrap(), None);
    }

    #[test]
    fn a_session_is_parked_only_while_that_is_the_last_thing_it_did() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        assert!(is_parked(&path).is_err());
        let mut log = SessionLog::create(&path).unwrap();
        assert!(!is_parked(&path).unwrap());

        log.append(&Entry::TurnParked).unwrap();
        assert!(is_parked(&path).unwrap());

        log.append(&Entry::TurnCancelled).unwrap();
        assert!(!is_parked(&path).unwrap());
    }
}
