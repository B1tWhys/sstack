//! The port through which the agent uses tools: what a tool is, what it is given for a Session,
//! and how the agent calls the ones it was given. The tools themselves live elsewhere and are
//! handed in by whoever sets the Session up, so core never names one.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use crate::commands::Skill;
use crate::connection::{Effort, ToolSpec};
use crate::protocol::{Question, Response, Verdict};
use crate::session::ToolCall;
use crate::workspace::Sandbox;

/// Someone who decides whether the model may run a command on the host, outside the Sandbox.
#[async_trait]
pub trait Approver: Send + Sync {
    /// Returns whether the model may run `command`, or that the question was put off. `note`,
    /// if given, tells the person why they are being asked.
    async fn approve(&self, command: &str, note: Option<&str>) -> Verdict;
}

/// Someone who answers the model's [`Question`]s.
#[async_trait]
pub trait Asker: Send + Sync {
    /// Puts `questions` to the person and waits for what becomes of them.
    async fn ask(&self, questions: Vec<Question>) -> Response;
}

/// An [`Approver`] that refuses everything, for runs with nobody to ask.
#[derive(Debug, Default)]
pub struct DenyAll;

#[async_trait]
impl Approver for DenyAll {
    async fn approve(&self, _command: &str, _note: Option<&str>) -> Verdict {
        Verdict::Deny
    }
}

/// Why a tool call has no ordinary result for the model.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// A failure reported back to the model in these words.
    #[error("{0}")]
    Failed(String),
    /// The call's question was put off, so it has no result yet and runs again when its Turn
    /// carries on.
    #[error("the question was put off until the Turn carries on")]
    Deferred,
}

/// A tool call that has no result yet, because the question it asked was put off. The call runs
/// again when its Turn carries on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deferred;

/// The result of a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// The output shown to the model.
    pub text: String,
    /// Whether the call failed.
    pub is_error: bool,
}

/// Something the model can call.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the definition offered to the model.
    fn spec(&self) -> ToolSpec;
    /// Runs the tool on `input`, returning what the model is told.
    async fn run(&self, input: Value) -> Result<String, ToolError>;
}

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

/// The [`SessionInfo`] a Session's tools read, which core keeps current when the Session moves to
/// another Connection.
pub type SharedSessionInfo = Arc<Mutex<SessionInfo>>;

/// What core knows about a Session that its tools may need.
#[derive(bon::Builder)]
pub struct ToolContext {
    /// The Workspace: the directory the model works in.
    pub root: PathBuf,
    /// Directories outside the Workspace that the model may read but not change, such as those
    /// of installed Skills.
    #[builder(default)]
    pub readable: Vec<PathBuf>,
    /// Where the Session's shell commands run without asking.
    pub sandbox: Arc<Sandbox>,
    /// Who decides on shell commands that would run on the host.
    pub approver: Arc<dyn Approver>,
    /// Who answers the model's Questions, if anyone does.
    pub asker: Option<Arc<dyn Asker>>,
    /// The Skills the model may load on its own.
    #[builder(default)]
    pub skills: Vec<Skill>,
    /// The main checkout, when the Session works in a worktree of its own and lands its work
    /// there. Missing for a Session in the main checkout.
    pub main_checkout: Option<PathBuf>,
    /// Environment variables holding secrets, withheld from anything the model runs.
    #[builder(default)]
    pub secret_env: Vec<String>,
    /// Facts about the Session, kept current as it moves between Connections.
    pub session_info: SharedSessionInfo,
}

/// Makes the tools for one Session from what core knows about it. Whoever sets Sessions up
/// supplies one, so that core never depends on the tools themselves.
pub type ToolFactory = dyn Fn(&ToolContext) -> io::Result<Vec<Box<dyn Tool>>> + Send + Sync;

/// Everything the model can do to the project during a Session.
pub struct ToolBox {
    /// The tools, in the order they are offered to the model.
    tools: Vec<Box<dyn Tool>>,
    /// The facts about the Session that its tools read.
    session_info: SharedSessionInfo,
}

impl ToolBox {
    /// Offers the model `tools`, in order, for the Session that `session_info` describes.
    #[must_use]
    pub fn new(tools: Vec<Box<dyn Tool>>, session_info: SharedSessionInfo) -> Self {
        Self {
            tools,
            session_info,
        }
    }

    /// Records that `model`, asked for `effort`, serves the Session from now on, so that tools
    /// telling the model about the Session say so.
    pub fn served_by(&self, model: &str, effort: Option<Effort>) {
        let mut info = (self.session_info.lock()).unwrap_or_else(PoisonError::into_inner);
        model.clone_into(&mut info.model);
        info.effort = effort;
    }

    /// Returns the definitions offered to the model, in order.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec()).collect()
    }

    /// Runs one tool call. Every failure becomes an error result for the model to react to.
    ///
    /// # Errors
    /// Returns [`Deferred`] if the call asked the person a question that was put off, so it has
    /// no result to record.
    pub async fn run(&self, call: &ToolCall) -> Result<ToolOutput, Deferred> {
        let tool = self.tools.iter().find(|tool| tool.spec().name == call.name);
        let result = match tool {
            Some(tool) => tool.run(call.input.clone()).await,
            None => Err(ToolError::Failed(format!("unknown tool: {}", call.name))),
        };
        match result {
            Ok(text) => Ok(ToolOutput {
                text,
                is_error: false,
            }),
            Err(ToolError::Failed(text)) => Ok(ToolOutput {
                text,
                is_error: true,
            }),
            Err(ToolError::Deferred) => Err(Deferred),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    //! Approvers and tools for tests that need a [`ToolBox`] but not the real tools.

    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::{Approver, SessionInfo, Tool, ToolBox, ToolError};
    use crate::connection::ToolSpec;
    use crate::protocol::Verdict;

    /// An [`Approver`] that lets every command run, as a person who always says yes.
    pub struct AllowAll;

    #[async_trait]
    impl Approver for AllowAll {
        async fn approve(&self, _command: &str, _note: Option<&str>) -> Verdict {
            Verdict::Allow
        }
    }

    /// An [`Approver`] that puts off every question, as a Rebuild does.
    pub struct DeferAll;

    #[async_trait]
    impl Approver for DeferAll {
        async fn approve(&self, _command: &str, _note: Option<&str>) -> Verdict {
            Verdict::Defer
        }
    }

    /// Returns a definition for a tool called `name` that takes anything.
    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("a fake {name}"),
            input_schema: json!({"type": "object"}),
        }
    }

    /// Returns the `path` a call was given, or nothing.
    fn path(input: &Value) -> &str {
        input["path"].as_str().unwrap_or_default()
    }

    /// A `read` that finds nothing.
    struct Read;

    #[async_trait]
    impl Tool for Read {
        fn spec(&self) -> ToolSpec {
            spec("read")
        }

        async fn run(&self, input: Value) -> Result<String, ToolError> {
            Err(ToolError::Failed(format!("not found: {}", path(&input))))
        }
    }

    /// A `write` that says it wrote, and writes nothing.
    struct Write;

    #[async_trait]
    impl Tool for Write {
        fn spec(&self) -> ToolSpec {
            spec("write")
        }

        async fn run(&self, input: Value) -> Result<String, ToolError> {
            Ok(format!("wrote {}", path(&input)))
        }
    }

    /// A `bash` that asks its Approver, then runs the command with `sh` in `root`, with no
    /// Sandbox. Dropping the call stops the command.
    struct Shell {
        /// Where commands run.
        root: PathBuf,
        /// Who decides whether each command runs.
        approver: Arc<dyn Approver>,
    }

    #[async_trait]
    impl Tool for Shell {
        fn spec(&self) -> ToolSpec {
            spec("bash")
        }

        async fn run(&self, input: Value) -> Result<String, ToolError> {
            let command = input["command"].as_str().unwrap_or_default();
            match self.approver.approve(command, None).await {
                Verdict::Allow => {}
                Verdict::Deny => return Err(ToolError::Failed("denied".into())),
                Verdict::Defer => return Err(ToolError::Deferred),
            }
            let output = tokio::process::Command::new("sh")
                .args(["-c", command])
                .current_dir(&self.root)
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
    }

    /// A [`ToolBox`] of fake `read`, `write` and `bash` tools for a Session working in `root`,
    /// whose commands `approver` decides on.
    pub fn fake_tools(root: &Path, approver: Arc<dyn Approver>) -> ToolBox {
        let shell = Shell {
            root: root.to_owned(),
            approver,
        };
        let info = SessionInfo {
            session_id: "1".into(),
            model: "fake".into(),
            effort: None,
        };
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(Read), Box::new(Write), Box::new(shell)];
        ToolBox::new(tools, Arc::new(Mutex::new(info)))
    }
}
