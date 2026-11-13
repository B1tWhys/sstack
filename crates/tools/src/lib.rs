//! The tools the model can use to work on a project, each an adapter on the port in
//! [`sstack::tools`]. A tool module uses only what this crate's root shares, never another tool.

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

use std::io;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

use project::Project;
use sstack::tools::{Tool, ToolContext, ToolError};

/// Parses a tool's JSON input into its input type.
fn parse_input<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input)
        .map_err(|error| ToolError::Failed(format!("invalid input: {error}")))
}

/// Makes every tool the Session in `context` may use, in the order they are offered to the model.
///
/// # Errors
/// Fails if the Workspace cannot be read.
pub fn standard(context: &ToolContext) -> io::Result<Vec<Box<dyn Tool>>> {
    let project = Project::new(&context.root)?.with_readable(context.readable.clone());
    let project = Arc::new(project);
    let seen = Arc::new(files::Seen::default());
    let bash = bash::Bash::builder()
        .project(project.clone())
        .sandbox(context.sandbox.clone())
        .approver(context.approver.clone())
        .secret_env(context.secret_env.clone())
        .build();
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(files::Read::new(project.clone(), seen.clone())),
        Box::new(files::Write::new(project.clone(), seen.clone())),
        Box::new(files::Edit::new(project.clone(), seen)),
        Box::new(search::Search::new(project.clone())),
        Box::new(bash),
        Box::new(session_info::ShowSessionInfo::new(
            context.session_info.clone(),
        )),
    ];
    if !context.skills.is_empty() {
        tools.push(Box::new(skill::UseSkill::new(context.skills.clone())));
    }
    if let Some(asker) = &context.asker {
        tools.push(Box::new(ask::Ask::new(asker.clone())));
    }
    if let Some(main) = &context.main_checkout {
        tools.push(Box::new(land::Land {
            worktree: project,
            main: main.clone(),
            approver: context.approver.clone(),
            secret_env: context.secret_env.clone(),
        }));
    }
    Ok(tools)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    pub use sstack::tools::test_support::{AllowAll, DeferAll};
    use sstack::tools::{Approver, SessionInfo, ToolBox, ToolContext};
    use sstack::workspace::Sandbox;
    pub use sstack::workspace::test_support::git;

    use super::project::Project;

    /// A Sandbox that never runs anything, so every shell command asks.
    pub fn disabled() -> Arc<Sandbox> {
        Arc::new(Sandbox::disabled())
    }

    /// A temporary git repository holding `.gitignore` rules for `ignored/` and `*.log`.
    pub fn git_project() -> (tempfile::TempDir, Arc<Project>) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join(".gitignore"), "ignored/\n*.log\n").unwrap();
        let project = Arc::new(Project::new(dir.path()).unwrap());
        (dir, project)
    }

    /// What a Session in the Workspace at `root` gives its tools, with a Sandbox that runs
    /// nothing, `approver` to decide on commands, and nobody to answer Questions.
    pub fn context(root: &Path, approver: Arc<dyn Approver>) -> ToolContext {
        ToolContext {
            root: root.to_owned(),
            readable: vec![],
            sandbox: disabled(),
            approver,
            asker: None,
            skills: vec![],
            main_checkout: None,
            secret_env: vec![],
            session_info: Arc::new(Mutex::new(SessionInfo {
                session_id: "42".into(),
                model: "m".into(),
                effort: None,
            })),
        }
    }

    /// The standard tools for a Session in the Workspace at `root`, as [`context`] describes it.
    pub fn toolbox(root: &Path, approver: Arc<dyn Approver>) -> ToolBox {
        let context = context(root, approver);
        ToolBox::new(super::standard(&context).unwrap(), context.session_info)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use async_trait::async_trait;
    use serde_json::json;

    use super::test_support::{DeferAll, context, git_project, toolbox};
    use super::*;
    use sstack::commands::{Scope, Skill};
    use sstack::connection::Effort;
    use sstack::protocol::{Question, Response};
    use sstack::session::ToolCall;
    use sstack::tools::{Asker, Deferred, DenyAll, ToolBox};

    /// Returns a call to `name` with `input`.
    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: name.into(),
            input,
        }
    }

    /// Returns the names of the tools `context` makes, in order.
    fn names(context: &ToolContext) -> Vec<String> {
        let tools = standard(context).unwrap();
        tools.iter().map(|tool| tool.spec().name).collect()
    }

    #[tokio::test]
    async fn dispatches_by_name_and_reports_failures_as_results() {
        let (dir, project) = git_project();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tools = toolbox(project.root(), Arc::new(DenyAll));

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
        let tools = toolbox(project.root(), Arc::new(DeferAll));

        let put_off = tools.run(&call("bash", json!({"command": "true"}))).await;

        assert_eq!(put_off, Err(Deferred));
    }

    #[test]
    fn offers_the_standard_tools() {
        let (_dir, project) = git_project();
        let context = context(project.root(), Arc::new(DenyAll));
        let expected = ["read", "write", "edit", "search", "bash", "session_info"];
        assert_eq!(names(&context), expected);
    }

    #[tokio::test]
    async fn the_session_info_stays_current_as_the_model_changes() {
        let (_dir, project) = git_project();
        let context = context(project.root(), Arc::new(DenyAll));
        let tools = ToolBox::new(standard(&context).unwrap(), context.session_info);
        let output = tools.run(&call("session_info", json!({}))).await.unwrap();
        assert_eq!(output.text, r#"{"session_id":"42","model":"m"}"#);

        tools.served_by("other", Some(Effort::Low));
        let output = tools.run(&call("session_info", json!({}))).await.unwrap();
        assert_eq!(
            output.text,
            r#"{"session_id":"42","model":"other","effort":"low"}"#
        );
    }

    #[test]
    fn offers_the_skill_tool_only_when_there_are_skills() {
        let (_dir, project) = git_project();
        let mut context = context(project.root(), Arc::new(DenyAll));
        assert!(!names(&context).contains(&"skill".to_owned()));

        context.skills = vec![Skill {
            name: "review".into(),
            description: String::new(),
            path: "SKILL.md".into(),
            scope: Scope::Project,
            model_invocable: true,
        }];
        assert!(names(&context).contains(&"skill".to_owned()));
    }

    #[test]
    fn offers_the_ask_tool_only_with_someone_to_ask() {
        /// An Asker that never answers.
        struct Silent;

        #[async_trait]
        impl Asker for Silent {
            async fn ask(&self, _questions: Vec<Question>) -> Response {
                Response::Declined
            }
        }

        let (_dir, project) = git_project();
        let mut context = context(project.root(), Arc::new(DenyAll));
        assert!(!names(&context).contains(&"ask".to_owned()));

        context.asker = Some(Arc::new(Silent));
        assert!(names(&context).contains(&"ask".to_owned()));
    }

    #[test]
    fn offers_the_land_tool_only_in_a_worktree_of_its_own() {
        let (_dir, project) = git_project();
        let mut context = context(project.root(), Arc::new(DenyAll));
        assert!(!names(&context).contains(&"land".to_owned()));

        context.main_checkout = Some(PathBuf::from("/main"));
        assert!(names(&context).contains(&"land".to_owned()));
    }
}
