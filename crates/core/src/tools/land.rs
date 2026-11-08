use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use super::project::Project;
use super::{Approver, Tool, ToolError};
use crate::provider::ToolSpec;
use crate::worktree::REPOSITORY_ENV;

/// How many times to try before giving up on a main branch that keeps moving.
const ATTEMPTS: usize = 3;
/// How long the checks may run. They can start from a cold build.
const HOOK_TIMEOUT: Duration = Duration::from_mins(30);
/// The most check output to return to the model.
const MAX_OUTPUT_CHARS: usize = 8_000;

/// Moves a worktree Session's commits onto the main branch, once the person approves.
///
/// Rebasing and fast-forwarding skip pre-commit hooks, so the checks run again on the rebased
/// result. Otherwise two changes that each pass could land as one that does not.
pub struct Land {
    /// The worktree the Session works in.
    pub worktree: Arc<Project>,
    /// The main checkout, which decides what the main branch is.
    pub main: PathBuf,
    /// The Session's branch.
    pub branch: String,
    /// Decides whether the landing may happen.
    pub approver: Arc<dyn Approver>,
    /// Environment variables withheld from the checks.
    pub secret_env: Vec<String>,
}

impl Land {
    /// Returns the branch to land on, which is whatever the main checkout has checked out.
    async fn target(&self) -> Result<String, ToolError> {
        let name = git(&self.main, &["symbolic-ref", "--short", "HEAD"]).await?;
        Ok(name.trim().to_owned())
    }

    /// Refuses to go on with uncommitted changes, which a rebase would trip over and landing
    /// would leave behind.
    async fn require_clean(&self) -> Result<(), ToolError> {
        let status = git(self.worktree.root(), &["status", "--porcelain"]).await?;
        if status.trim().is_empty() {
            Ok(())
        } else {
            Err(ToolError(format!(
                "commit or discard these changes first:\n{status}"
            )))
        }
    }

    /// Returns whether the Session has commits that `target` does not.
    async fn has_commits(&self, target: &str) -> Result<bool, ToolError> {
        let range = format!("{target}..HEAD");
        let count = git(self.worktree.root(), &["rev-list", "--count", &range]).await?;
        Ok(count.trim() != "0")
    }

    /// Replays the Session's commits on top of `target`. A conflict leaves the worktree as it was,
    /// so the model can run the rebase itself and resolve it.
    async fn rebase(&self, target: &str) -> Result<(), ToolError> {
        let root = self.worktree.root();
        if git(root, &["rebase", target]).await.is_ok() {
            return Ok(());
        }
        let conflicts = git(root, &["diff", "--name-only", "--diff-filter=U"]).await?;
        git(root, &["rebase", "--abort"]).await?;
        Err(ToolError(format!(
            "rebasing onto {target} conflicts in:\n{conflicts}\nrun `git rebase {target}`, \
             resolve the conflicts, then land again"
        )))
    }

    /// Runs the project's pre-commit checks on the worktree.
    async fn check(&self) -> Result<(), ToolError> {
        let mut command = git_command(self.worktree.root());
        command.args(["hook", "run", "--ignore-missing", "pre-commit"]);
        for name in &self.secret_env {
            command.env_remove(name);
        }
        let output = tokio::time::timeout(HOOK_TIMEOUT, command.output())
            .await
            .map_err(|_| ToolError(format!("the checks timed out after {HOOK_TIMEOUT:?}")))?
            .map_err(|error| ToolError(format!("failed to start git: {error}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(ToolError(format!(
                "the checks failed, so nothing landed:\n{}",
                tail(&combined(&output))
            )))
        }
    }

    /// Moves `target` up to the Session's commits, returning false if it moved on while the checks
    /// ran, in which case it is left alone.
    async fn fast_forward(&self, target: &str) -> Result<bool, ToolError> {
        let ancestor = ["merge-base", "--is-ancestor", target, "HEAD"];
        if git(self.worktree.root(), &ancestor).await.is_err() {
            return Ok(false);
        }
        git(&self.main, &["merge", "-q", "--ff-only", &self.branch]).await?;
        Ok(true)
    }

    /// Lands the Session's commits on `target`, starting again if it moves in the meantime.
    async fn land_on(&self, target: &str) -> Result<String, ToolError> {
        for _ in 0..ATTEMPTS {
            self.rebase(target).await?;
            self.check().await?;
            if self.fast_forward(target).await? {
                return Ok(format!("landed {} on {target}", self.branch));
            }
        }
        Err(ToolError(format!(
            "{target} kept moving: try again in a moment"
        )))
    }
}

/// Returns everything a finished command printed.
fn combined(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}{stderr}")
}

/// Keeps the end of long output, where failures usually are.
fn tail(text: &str) -> String {
    let total = text.chars().count();
    text.chars()
        .skip(total.saturating_sub(MAX_OUTPUT_CHARS))
        .collect()
}

/// Returns a git process ready to run in `directory`, unaffected by any hook sstack runs under.
fn git_command(directory: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for name in REPOSITORY_ENV {
        command.env_remove(name);
    }
    command
}

/// Runs git in `directory`, returning what it printed.
async fn git(directory: &Path, args: &[&str]) -> Result<String, ToolError> {
    let mut command = git_command(directory);
    let output = command
        .args(args)
        .output()
        .await
        .map_err(|error| ToolError(format!("failed to start git: {error}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(ToolError(combined(&output).trim().to_owned()))
    }
}

#[async_trait]
impl Tool for Land {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "land".into(),
            description: "Move this Session's commits onto the main branch: rebases them, reruns \
                          the project's checks, then fast-forwards. Never pushes. The user \
                          approves each landing."
                .into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    async fn run(&self, _input: Value) -> Result<String, ToolError> {
        self.require_clean().await?;
        let target = self.target().await?;
        if !self.has_commits(&target).await? {
            return Ok(format!("nothing to land: {target} has everything"));
        }
        let request = format!("land {} onto {target}", self.branch);
        if !self.approver.approve(&request, None).await {
            return Err(ToolError("the user denied this landing".to_owned()));
        }
        self.land_on(&target).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{AllowAll, git as run};
    use crate::tools::{DenyAll, ToolBox};
    use crate::worktree;
    use crate::worktree::test_support::repository;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// A repository with one Session worktree, and the tool that lands it.
    struct Fixture {
        dir: tempfile::TempDir,
        worktree: PathBuf,
        land: Land,
    }

    fn fixture(approver: Arc<dyn Approver>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let worktree = worktree::create(dir.path(), "1").unwrap();
        let land = Land {
            worktree: Arc::new(Project::new(&worktree).unwrap()),
            main: dir.path().to_owned(),
            branch: worktree::branch("1"),
            approver,
            secret_env: vec![],
        };
        Fixture {
            dir,
            worktree,
            land,
        }
    }

    /// Commits a file called `name` holding `content` in `root`.
    fn commit(root: &Path, name: &str, content: &str) {
        fs::write(root.join(name), content).unwrap();
        run(root, &["add", "."]);
        run(
            root,
            &["-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", name],
        );
    }

    fn hook(root: &Path, script: &str) {
        let path = root.join(".git/hooks/pre-commit");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn commits_land_on_the_main_branch() {
        let f = fixture(Arc::new(AllowAll));
        commit(&f.worktree, "a.txt", "a");

        let landed = f.land.run(json!({})).await.unwrap();

        assert_eq!(landed, "landed session/1 on main");
        assert!(f.dir.path().join("a.txt").exists());
    }

    #[tokio::test]
    async fn work_landed_by_another_session_is_rebased_over() {
        let f = fixture(Arc::new(AllowAll));
        commit(&f.worktree, "a.txt", "a");
        commit(f.dir.path(), "b.txt", "b");

        f.land.run(json!({})).await.unwrap();

        assert!(f.dir.path().join("a.txt").exists());
        assert!(f.dir.path().join("b.txt").exists());
        assert!(f.worktree.join("b.txt").exists());
    }

    #[tokio::test]
    async fn nothing_to_land_is_not_a_failure_and_asks_nothing() {
        let f = fixture(Arc::new(DenyAll));
        let said = f.land.run(json!({})).await.unwrap();
        assert!(said.starts_with("nothing to land"), "{said}");
    }

    #[tokio::test]
    async fn uncommitted_changes_must_be_dealt_with_first() {
        let f = fixture(Arc::new(AllowAll));
        fs::write(f.worktree.join("a.txt"), "a").unwrap();
        let ToolError(said) = f.land.run(json!({})).await.unwrap_err();
        assert!(said.contains("a.txt"), "{said}");
    }

    #[tokio::test]
    async fn the_person_can_deny_a_landing() {
        let f = fixture(Arc::new(DenyAll));
        commit(&f.worktree, "a.txt", "a");
        assert!(f.land.run(json!({})).await.is_err());
        assert!(!f.dir.path().join("a.txt").exists());
    }

    #[tokio::test]
    async fn failing_checks_stop_the_landing_and_are_reported() {
        let f = fixture(Arc::new(AllowAll));
        commit(&f.worktree, "a.txt", "a");
        hook(f.dir.path(), "echo clippy is unhappy\nexit 1");

        let ToolError(said) = f.land.run(json!({})).await.unwrap_err();

        assert!(said.contains("clippy is unhappy"), "{said}");
        assert!(!f.dir.path().join("a.txt").exists());
    }

    #[tokio::test]
    async fn checks_see_the_rebased_result_in_the_worktree() {
        let f = fixture(Arc::new(AllowAll));
        commit(&f.worktree, "a.txt", "a");
        commit(f.dir.path(), "b.txt", "b");
        hook(f.dir.path(), "test -f a.txt && test -f b.txt");

        f.land.run(json!({})).await.unwrap();

        assert!(f.dir.path().join("a.txt").exists());
    }

    #[tokio::test]
    async fn a_conflict_leaves_the_worktree_ready_for_the_model() {
        let f = fixture(Arc::new(AllowAll));
        commit(&f.worktree, "a.txt", "mine");
        commit(f.dir.path(), "a.txt", "theirs");

        let ToolError(said) = f.land.run(json!({})).await.unwrap_err();

        assert!(said.contains("a.txt"), "{said}");
        assert!(said.contains("git rebase main"), "{said}");
        let status = git(&f.worktree, &["status", "--porcelain"]).await.unwrap();
        assert_eq!(status, "");
    }

    #[tokio::test]
    async fn a_main_branch_that_moves_during_the_checks_is_rebased_over_again() {
        let f = fixture(Arc::new(AllowAll));
        commit(&f.worktree, "a.txt", "a");
        let main = f.dir.path().display();
        hook(
            f.dir.path(),
            &format!(
                "if [ ! -f {main}/moved ]; then touch {main}/moved; \
                 unset GIT_DIR GIT_INDEX_FILE; \
                 git -C {main} commit -q --allow-empty --no-verify -m moved; fi"
            ),
        );

        f.land.run(json!({})).await.unwrap();

        assert!(f.dir.path().join("a.txt").exists());
        let log = git(f.dir.path(), &["log", "--format=%s"]).await.unwrap();
        assert_eq!(log, "a.txt\nmoved\nfirst\n");
    }

    #[tokio::test]
    async fn a_main_branch_that_never_settles_is_given_up_on() {
        let f = fixture(Arc::new(AllowAll));
        commit(&f.worktree, "a.txt", "a");
        let main = f.dir.path().display();
        hook(
            f.dir.path(),
            &format!(
                "unset GIT_DIR GIT_INDEX_FILE; \
                 git -C {main} commit -q --allow-empty --no-verify -m moved"
            ),
        );

        let ToolError(said) = f.land.run(json!({})).await.unwrap_err();

        assert!(said.contains("kept moving"), "{said}");
    }

    #[test]
    fn long_check_output_keeps_its_end() {
        let long = format!("{}end", "x".repeat(MAX_OUTPUT_CHARS));
        let kept = tail(&long);
        assert_eq!(kept.chars().count(), MAX_OUTPUT_CHARS);
        assert!(kept.ends_with("end"));
    }

    #[test]
    fn the_tool_is_offered_alongside_the_standard_ones() {
        let f = fixture(Arc::new(DenyAll));
        let project = f.land.worktree.clone();
        let tools = ToolBox::standard()
            .project(project)
            .sandbox(crate::tools::test_support::disabled())
            .approver(Arc::new(DenyAll))
            .call()
            .with_land(f.land);
        let last = tools.specs().pop().unwrap();
        assert_eq!(last.name, "land");
    }
}
