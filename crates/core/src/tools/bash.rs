use std::io;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use super::project::Project;
use super::{Approver, Tool, ToolError, parse_input};
use crate::provider::ToolSpec;

const TIMEOUT: Duration = Duration::from_mins(10);
const MAX_OUTPUT_CHARS: usize = 30_000;

pub(super) struct Bash {
    project: Arc<Project>,
    approver: Arc<dyn Approver>,
    secret_env: Vec<String>,
}

impl Bash {
    pub(super) fn new(
        project: Arc<Project>,
        approver: Arc<dyn Approver>,
        secret_env: Vec<String>,
    ) -> Self {
        Self {
            project,
            approver,
            secret_env,
        }
    }

    fn command(&self, script: &str) -> Command {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(script)
            .current_dir(self.project.root())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for name in &self.secret_env {
            command.env_remove(name);
        }
        command
    }
}

/// Keeps the end of long output, where errors and summaries usually are.
fn truncate(text: &str) -> String {
    let total = text.chars().count();
    if total <= MAX_OUTPUT_CHARS {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(total - MAX_OUTPUT_CHARS).collect();
    format!(
        "... {} characters omitted\n{tail}",
        total - MAX_OUTPUT_CHARS
    )
}

fn render(output: &Output) -> Result<String, ToolError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = truncate(&format!("{stdout}{stderr}"));
    if output.status.success() {
        Ok(text)
    } else {
        Err(ToolError(format!("{}\n{text}", output.status)))
    }
}

async fn finish(
    output: impl Future<Output = io::Result<Output>>,
    limit: Duration,
) -> Result<Output, ToolError> {
    match tokio::time::timeout(limit, output).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(ToolError(format!("failed to start bash: {error}"))),
        Err(_) => Err(ToolError(format!("timed out after {limit:?}"))),
    }
}

#[derive(Deserialize)]
struct BashInput {
    command: String,
}

#[async_trait]
impl Tool for Bash {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command in the project root. The user approves each one."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let input: BashInput = parse_input(input)?;
        if !self.approver.approve(&input.command).await {
            return Err(ToolError("the user denied this command".to_owned()));
        }
        render(&finish(self.command(&input.command).output(), TIMEOUT).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::DenyAll;
    use crate::tools::test_support::{AllowAll, git_project};

    fn bash(project: Arc<Project>, secret_env: Vec<String>) -> Bash {
        Bash::new(project, Arc::new(AllowAll), secret_env)
    }

    #[tokio::test]
    async fn runs_in_the_project_root_and_combines_output() {
        let (dir, project) = git_project();
        let expected = dir.path().canonicalize().unwrap();
        let output = bash(project, vec![])
            .run(json!({"command": "pwd -P; echo oops >&2"}))
            .await
            .unwrap();
        assert_eq!(output, format!("{}\noops\n", expected.display()));
    }

    #[tokio::test]
    async fn a_failing_command_is_an_error_that_keeps_its_output() {
        let (_dir, project) = git_project();
        let error = bash(project, vec![])
            .run(json!({"command": "echo nope; exit 3"}))
            .await
            .unwrap_err();
        assert!(error.0.contains("nope"));
        assert!(error.0.contains('3'));
    }

    #[tokio::test]
    async fn denied_commands_never_run() {
        let (dir, project) = git_project();
        let tool = Bash::new(project, Arc::new(DenyAll), vec![]);
        let error = tool.run(json!({"command": "touch ran"})).await.unwrap_err();
        assert_eq!(error.0, "the user denied this command");
        assert!(!dir.path().join("ran").exists());
    }

    #[tokio::test]
    async fn secrets_are_withheld_from_commands() {
        let (_dir, project) = git_project();
        let output = bash(project, vec!["HOME".to_owned()])
            .run(json!({"command": "echo \"[$HOME]\""}))
            .await
            .unwrap();
        assert_eq!(output, "[]\n");
    }

    #[tokio::test]
    async fn slow_and_unstartable_commands_become_errors() {
        let limit = Duration::from_millis(1);
        let slow = finish(std::future::pending(), limit).await.unwrap_err();
        let failed = finish(async { Err(io::Error::other("gone")) }, limit)
            .await
            .unwrap_err();
        assert_eq!(slow.0, "timed out after 1ms");
        assert_eq!(failed.0, "failed to start bash: gone");
    }

    #[test]
    fn long_output_keeps_its_tail() {
        let long = format!("{}end", "x".repeat(MAX_OUTPUT_CHARS));
        let short = truncate(&long);
        assert!(short.starts_with("... 3 characters omitted\n"));
        assert!(short.ends_with("end"));
        assert_eq!(truncate("short"), "short");
    }
}
