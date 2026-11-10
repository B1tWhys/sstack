//! The tools the model can use to work on a project.

/// The tool that puts Questions to the person.
mod ask;
/// The tool that runs shell commands.
mod bash;
/// The tools that read and change files.
mod files;
/// The tool that moves a worktree Session's commits onto the main branch.
mod land;
pub mod project;
/// The tool that finds files and text.
mod search;
/// The tool that tells the model about the Session it is in.
mod session_info;
/// The tool that hands the model a Skill's instructions.
mod skill;

use std::sync::Arc;

use async_trait::async_trait;
use bon::bon;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::provider::ToolSpec;
use crate::question::Asker;
use crate::sandbox::Sandbox;
use crate::session::ToolCall;
use crate::skills::Skill;
pub use land::Land;
use project::Project;
pub use session_info::SessionInfo;

/// Someone who decides whether the model may run a command on the host, outside the Sandbox.
#[async_trait]
pub trait Approver: Send + Sync {
    /// Returns whether the model may run `command`, or that the question was put off. `note`,
    /// if given, tells the person why they are being asked.
    async fn approve(&self, command: &str, note: Option<&str>) -> Verdict;
}

/// What became of a question whether the model may run a command on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The command may run.
    Allow,
    /// The command may not run, and the model is told so.
    Deny,
    /// Nobody decided, because a Rebuild put the question off. The call that asked runs again,
    /// asking again, when its Turn carries on.
    Defer,
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
trait Tool: Send + Sync {
    /// Returns the definition offered to the model.
    fn spec(&self) -> ToolSpec;
    async fn run(&self, input: Value) -> Result<String, ToolError>;
}

/// Parses a tool's JSON input into its input type.
fn parse_input<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input)
        .map_err(|error| ToolError::Failed(format!("invalid input: {error}")))
}

/// Everything the model can do to the project during a Session.
pub struct ToolBox {
    /// The tools, in the order they are offered to the model.
    tools: Vec<Box<dyn Tool>>,
}

#[bon]
impl ToolBox {
    /// Builds the standard tools for a project.
    #[builder]
    pub fn standard(
        /// The project the tools work on.
        project: Arc<Project>,
        /// Where shell commands run without asking.
        sandbox: Arc<Sandbox>,
        /// Who decides on shell commands that would run on the host.
        approver: Arc<dyn Approver>,
        /// Environment variables withheld from shell commands run on the host.
        #[builder(default)]
        secret_env: Vec<String>,
    ) -> Self {
        let seen = Arc::new(files::Seen::default());
        let bash = bash::Bash::builder()
            .project(project.clone())
            .sandbox(sandbox)
            .approver(approver)
            .secret_env(secret_env)
            .build();
        Self {
            tools: vec![
                Box::new(files::Read::new(project.clone(), seen.clone())),
                Box::new(files::Write::new(project.clone(), seen.clone())),
                Box::new(files::Edit::new(project.clone(), seen)),
                Box::new(search::Search::new(project)),
                Box::new(bash),
            ],
        }
    }
}

impl ToolBox {
    /// Adds the ability to land commits, for a Session that works in a worktree.
    #[must_use]
    pub fn with_land(mut self, land: Land) -> Self {
        self.tools.push(Box::new(land));
        self
    }

    /// Adds the ability to learn `info` about the Session, so that the model can record in its
    /// commits which Session made them and with which model.
    #[must_use]
    pub fn with_session_info(mut self, info: SessionInfo) -> Self {
        self.tools
            .push(Box::new(session_info::ShowSessionInfo::new(info)));
        self
    }

    /// Adds the ability to read the instructions of `skills`. With none, offers nothing new.
    #[must_use]
    pub fn with_skills(mut self, skills: Vec<Skill>) -> Self {
        if !skills.is_empty() {
            self.tools.push(Box::new(skill::UseSkill::new(skills)));
        }
        self
    }

    /// Adds the ability to put Questions to the person through `asker`. With none, as when
    /// nobody is there to answer, offers nothing new.
    #[must_use]
    pub fn with_asker(mut self, asker: Option<Arc<dyn Asker>>) -> Self {
        if let Some(asker) = asker {
            self.tools.push(Box::new(ask::Ask::new(asker)));
        }
        self
    }

    /// Returns the definitions offered to the model.
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

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::project::Project;
    use super::{Approver, Verdict};

    use crate::sandbox::Sandbox;

    /// A Sandbox that never runs anything, so every shell command asks.
    pub fn disabled() -> Arc<Sandbox> {
        Arc::new(Sandbox::disabled())
    }

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
    use super::test_support::{disabled, git_project};
    use super::*;
    use crate::skills::Scope;
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
        let tools = ToolBox::standard()
            .project(project)
            .sandbox(disabled())
            .approver(Arc::new(DenyAll))
            .call();

        let run = async |name, input| tools.run(&call(name, input)).await.unwrap();
        let read = run("read", json!({"path": "a.txt"})).await;
        let unknown = run("nope", json!({})).await;
        let invalid = run("read", json!({"wrong": 1})).await;
        let denied = run("bash", json!({"command": "true"})).await;

        assert_eq!(read.text, "1\thello");
        assert!(!read.is_error);
        assert_eq!(unknown.text, "unknown tool: nope");
        assert!(unknown.is_error);
        assert!(invalid.is_error);
        assert!(denied.is_error);
    }

    #[tokio::test]
    async fn a_call_whose_question_was_put_off_has_no_result() {
        let (_dir, project) = git_project();
        let tools = ToolBox::standard()
            .project(project)
            .sandbox(disabled())
            .approver(Arc::new(super::test_support::DeferAll))
            .call();

        let put_off = tools.run(&call("bash", json!({"command": "true"}))).await;

        assert_eq!(put_off, Err(Deferred));
    }

    #[test]
    fn offers_the_standard_tools() {
        let (_dir, project) = git_project();
        let tools = ToolBox::standard()
            .project(project)
            .sandbox(disabled())
            .approver(Arc::new(DenyAll))
            .call();
        let names: Vec<_> = tools.specs().into_iter().map(|spec| spec.name).collect();
        assert_eq!(names, ["read", "write", "edit", "search", "bash"]);
    }

    #[tokio::test]
    async fn offers_the_session_info_when_given_some() {
        let (_dir, project) = git_project();
        let tools = ToolBox::standard()
            .project(project)
            .sandbox(disabled())
            .approver(Arc::new(DenyAll))
            .call();
        assert!(!tools.specs().iter().any(|spec| spec.name == "session_info"));

        let tools = tools.with_session_info(SessionInfo {
            session_id: "42".into(),
            model: "m".into(),
            effort: None,
        });
        let output = tools.run(&call("session_info", json!({}))).await.unwrap();

        assert_eq!(output.text, r#"{"session_id":"42","model":"m"}"#);
    }

    #[test]
    fn offers_the_skill_tool_only_when_there_are_skills() {
        let (_dir, project) = git_project();
        let tools = ToolBox::standard()
            .project(project)
            .sandbox(disabled())
            .approver(Arc::new(DenyAll))
            .call()
            .with_skills(vec![]);
        assert!(!tools.specs().iter().any(|spec| spec.name == "skill"));

        let skill = Skill {
            name: "review".into(),
            description: String::new(),
            path: "SKILL.md".into(),
            scope: Scope::Project,
            model_invocable: true,
        };
        let tools = tools.with_skills(vec![skill]);
        assert!(tools.specs().iter().any(|spec| spec.name == "skill"));
    }

    #[test]
    fn offers_the_ask_tool_only_with_someone_to_ask() {
        /// An Asker that never answers.
        struct Silent;

        #[async_trait]
        impl Asker for Silent {
            async fn ask(
                &self,
                _questions: Vec<crate::question::Question>,
            ) -> Option<Vec<crate::question::Answer>> {
                None
            }
        }

        let (_dir, project) = git_project();
        let tools = ToolBox::standard()
            .project(project)
            .sandbox(disabled())
            .approver(Arc::new(DenyAll))
            .call()
            .with_asker(None);
        assert!(!tools.specs().iter().any(|spec| spec.name == "ask"));

        let tools = tools.with_asker(Some(Arc::new(Silent)));
        assert!(tools.specs().iter().any(|spec| spec.name == "ask"));
    }
}
