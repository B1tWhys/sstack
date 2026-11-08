use std::io;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bon::Builder;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use super::project::Project;
use super::{Approver, Tool, ToolError, parse_input};
use crate::provider::ToolSpec;
use crate::sandbox::{Failure, Sandbox};

/// How long a command may run before it is stopped.
const TIMEOUT: Duration = Duration::from_mins(10);
/// The most output to return to the model from one command.
const MAX_OUTPUT_CHARS: usize = 30_000;
/// What the approval prompt says about a command the model chose to run outside the Sandbox.
const ASKED_NOTE: &str = "The model asked to run this outside the Sandbox.";

/// The tool for running shell commands in the project: in the Sandbox without asking,
/// or on the host once the person approves.
#[derive(Builder)]
pub(super) struct Bash {
    /// The project commands run in.
    project: Arc<Project>,
    /// Where commands run without asking.
    sandbox: Arc<Sandbox>,
    /// Who decides whether each command may run on the host.
    approver: Arc<dyn Approver>,
    /// Environment variables withheld from commands run on the host.
    #[builder(default)]
    secret_env: Vec<String>,
}

impl Bash {
    /// Runs `script` in the Sandbox, or on the host with the person's approval if the Sandbox
    /// is unavailable.
    async fn sandboxed(&self, script: &str) -> Result<String, ToolError> {
        match self.sandbox.run(script, TIMEOUT).await {
            Ok(output) => render(&output),
            Err(Failure::TimedOut(limit)) => Err(ToolError(format!("timed out after {limit:?}"))),
            Err(Failure::Unavailable(why)) => {
                let note = format!("The Sandbox is unavailable, so this runs on the host: {why}");
                self.on_host(script, &note).await
            }
        }
    }

    /// Runs `script` on the host once the person approves, telling them `note`.
    async fn on_host(&self, script: &str, note: &str) -> Result<String, ToolError> {
        if !self.approver.approve(script, Some(note)).await {
            return Err(ToolError("the user denied this command".to_owned()));
        }
        render(&finish(self.command(script).output(), TIMEOUT).await?)
    }

    /// Builds the process for one command on the host, without starting it.
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

/// Turns a finished command into model-facing output, or an error if it failed.
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

/// Waits for a command's output for up to `limit`.
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

/// The input to the `bash` tool.
#[derive(Deserialize)]
struct BashInput {
    /// The shell command to run.
    command: String,
    /// Whether to run on the host, outside the Sandbox, which the person must approve.
    #[serde(default)]
    unsandboxed: bool,
}

#[async_trait]
impl Tool for Bash {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a bash command in the project root. It runs in a sandbox: a \
                container that sees only the project, has no network and cannot see .git or \
                .env files, so git does not work there. It has coreutils, grep, sed, awk, find, \
                rg, fd, jq, python3, patch, file, tree, xxd, bc and less, but no compilers. \
                Shell pipelines are fine for searching and for mechanical edits across many \
                files. For a single targeted change, use the edit tool. Set unsandboxed to \
                true only for a command that needs the network, git, a toolchain or files \
                outside the project; the user approves each of those."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "unsandboxed": {
                        "type": "boolean",
                        "description": "Run on the host instead of in the sandbox, once the \
                            user approves."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let input: BashInput = parse_input(input)?;
        if input.unsandboxed {
            self.on_host(&input.command, ASKED_NOTE).await
        } else {
            self.sandboxed(&input.command).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{Containers, MockContainerRuntime};
    use crate::tools::DenyAll;
    use crate::tools::test_support::{AllowAll, disabled, git_project};
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::Mutex;

    fn bash(project: Arc<Project>, secret_env: Vec<String>) -> Bash {
        Bash::builder()
            .project(project)
            .sandbox(disabled())
            .approver(Arc::new(AllowAll))
            .secret_env(secret_env)
            .build()
    }

    fn tool(project: Arc<Project>, sandbox: Arc<Sandbox>, approver: Arc<dyn Approver>) -> Bash {
        Bash::builder()
            .project(project)
            .sandbox(sandbox)
            .approver(approver)
            .build()
    }

    /// An approver of everything, which remembers the note it was shown each time.
    #[derive(Default)]
    struct Noting(Mutex<Vec<Option<String>>>);

    #[async_trait]
    impl Approver for Noting {
        async fn approve(&self, _command: &str, note: Option<&str>) -> bool {
            self.0.lock().unwrap().push(note.map(str::to_owned));
            true
        }
    }

    /// A Sandbox whose every command exits with `code` after printing `stdout`.
    /// It writes files as the person, as the Sandbox checks before running anything.
    fn sandbox(project: &Project, code: i32, stdout: &'static str) -> Arc<Sandbox> {
        let mut runtime = MockContainerRuntime::new();
        runtime.expect_available().return_const(true);
        runtime.expect_sandboxes().returning(Vec::new);
        runtime.expect_has_image().return_const(true);
        runtime.expect_inspect().return_const(None);
        runtime.expect_start().returning(|_| Ok(()));
        runtime.expect_exec().returning(move |_, argv| {
            if argv[0] == "touch" {
                std::fs::write(&argv[1], "").unwrap();
            }
            Ok(Output {
                status: ExitStatus::from_raw(code << 8),
                stdout: stdout.into(),
                stderr: Vec::new(),
            })
        });
        runtime.expect_remove_detached().return_const(());
        let containers = Arc::new(Containers::new(vec![Arc::new(runtime)]));
        Arc::new(Sandbox::new(containers, "7", project.root()))
    }

    #[tokio::test]
    async fn sandboxed_commands_run_without_asking() {
        let (_dir, project) = git_project();
        let tool = tool(
            project.clone(),
            sandbox(&project, 0, "inside\n"),
            Arc::new(DenyAll),
        );
        let output = tool.run(json!({"command": "ls"})).await.unwrap();
        assert_eq!(output, "inside\n");
    }

    #[tokio::test]
    async fn a_sandboxed_command_past_its_limit_times_out() {
        let (_dir, project) = git_project();
        let tool = tool(
            project.clone(),
            sandbox(&project, 124, ""),
            Arc::new(DenyAll),
        );
        let error = tool.run(json!({"command": "sleep 999"})).await.unwrap_err();
        assert_eq!(error.0, "timed out after 600s");
    }

    #[tokio::test]
    async fn running_on_the_host_asks_and_says_why() {
        let (dir, project) = git_project();
        let noting = Arc::new(Noting::default());
        let asked = tool(project.clone(), sandbox(&project, 0, ""), noting.clone());
        let script = json!({"command": "touch host", "unsandboxed": true});
        asked.run(script).await.unwrap();
        let (_other, elsewhere) = git_project();
        let fallback = tool(elsewhere, disabled(), noting.clone());
        fallback.run(json!({"command": "true"})).await.unwrap();

        assert!(dir.path().join("host").exists());
        let notes = noting.0.lock().unwrap().clone();
        assert_eq!(notes[0].as_deref(), Some(ASKED_NOTE));
        let unavailable = notes[1].as_deref().unwrap();
        assert!(unavailable.starts_with("The Sandbox is unavailable, so this runs on the host: "));
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
        let tool = tool(project, disabled(), Arc::new(DenyAll));
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
