//! The tools the model can use to work on a project.

/// The tool that runs shell commands.
mod bash;
/// The tools that read and change files.
mod files;
/// The tool that moves a worktree Session's commits onto the main branch.
mod land;
pub mod project;
/// The tool that finds files and text.
mod search;

use std::sync::Arc;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::provider::ToolSpec;
use crate::session::ToolCall;
pub use land::Land;
use project::Project;

/// Decides whether the model may run a shell command.
#[async_trait]
pub trait Approver: Send + Sync {
    /// Returns whether the model may run `command`.
    async fn approve(&self, command: &str) -> bool;
}

/// Refuses everything, for runs with nobody to ask.
#[derive(Debug, Default)]
pub struct DenyAll;

#[async_trait]
impl Approver for DenyAll {
    async fn approve(&self, _command: &str) -> bool {
        false
    }
}

/// A failure reported back to the model.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ToolError(pub String);

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
trait Tool: Send + Sync {
    /// Returns the definition offered to the model.
    fn spec(&self) -> ToolSpec;
    async fn run(&self, input: Value) -> Result<String, ToolError>;
}

/// Parses a tool's JSON input into its input type.
fn parse_input<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|error| ToolError(format!("invalid input: {error}")))
}

/// Everything the model can do to the project during a Session.
pub struct ToolBox {
    /// The tools, in the order they are offered to the model.
    tools: Vec<Box<dyn Tool>>,
}

impl ToolBox {
    /// Builds the standard tools for `project`. Variables named in `secret_env` are withheld
    /// from shell commands.
    #[must_use]
    pub fn standard(
        project: Arc<Project>,
        approver: Arc<dyn Approver>,
        secret_env: Vec<String>,
    ) -> Self {
        Self {
            tools: vec![
                Box::new(files::Read::new(project.clone())),
                Box::new(files::Write::new(project.clone())),
                Box::new(files::Edit::new(project.clone())),
                Box::new(search::Search::new(project.clone())),
                Box::new(bash::Bash::new(project, approver, secret_env)),
            ],
        }
    }

    /// Adds the ability to land commits, for a Session that works in a worktree.
    #[must_use]
    pub fn with_land(mut self, land: Land) -> Self {
        self.tools.push(Box::new(land));
        self
    }

    /// Returns the definitions offered to the model.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec()).collect()
    }

    /// Runs one tool call. Every failure becomes an error result for the model to react to.
    pub async fn run(&self, call: &ToolCall) -> ToolOutput {
        let tool = self.tools.iter().find(|tool| tool.spec().name == call.name);
        let result = match tool {
            Some(tool) => tool.run(call.input.clone()).await,
            None => Err(ToolError(format!("unknown tool: {}", call.name))),
        };
        match result {
            Ok(text) => ToolOutput {
                text,
                is_error: false,
            },
            Err(ToolError(text)) => ToolOutput {
                text,
                is_error: true,
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::Approver;
    use super::project::Project;

    pub struct AllowAll;

    #[async_trait]
    impl Approver for AllowAll {
        async fn approve(&self, _command: &str) -> bool {
            true
        }
    }

    /// A temporary git repository holding `.gitignore` rules for `ignored/` and `*.log`.
    pub fn git_project() -> (tempfile::TempDir, Arc<Project>) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join(".gitignore"), "ignored/\n*.log\n").unwrap();
        let project = Arc::new(Project::new(dir.path()).unwrap());
        (dir, project)
    }

    pub fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        command.arg("-C").arg(root).args(args);
        for name in crate::worktree::REPOSITORY_ENV {
            command.env_remove(name);
        }
        assert!(command.status().unwrap().success());
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::git_project;
    use super::*;
    use serde_json::json;

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: name.into(),
            input,
        }
    }

    #[tokio::test]
    async fn dispatches_by_name_and_reports_failures_as_results() {
        let (dir, project) = git_project();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tools = ToolBox::standard(project, Arc::new(DenyAll), vec![]);

        let read = tools.run(&call("read", json!({"path": "a.txt"}))).await;
        let unknown = tools.run(&call("nope", json!({}))).await;
        let invalid = tools.run(&call("read", json!({"wrong": 1}))).await;
        let denied = tools.run(&call("bash", json!({"command": "true"}))).await;

        assert_eq!(read.text, "1\thello");
        assert!(!read.is_error);
        assert_eq!(unknown.text, "unknown tool: nope");
        assert!(unknown.is_error);
        assert!(invalid.is_error);
        assert!(denied.is_error);
    }

    #[test]
    fn offers_the_standard_tools() {
        let (_dir, project) = git_project();
        let tools = ToolBox::standard(project, Arc::new(DenyAll), vec![]);
        let names: Vec<_> = tools.specs().into_iter().map(|spec| spec.name).collect();
        assert_eq!(names, ["read", "write", "edit", "search", "bash"]);
    }
}
