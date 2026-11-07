//! Giving a Session its own copy of the project, so that parallel Sessions cannot disturb each
//! other's half-finished work.
//!
//! A Session has a worktree exactly when the directory [`path`] names exists. Any other Session
//! works in the main checkout.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The directory under the main checkout where sstack keeps its files.
const DATA_DIR: &str = ".sstack";
/// The line that keeps `DATA_DIR` out of git, wherever a project's ignore rules live.
const EXCLUDE_LINE: &str = ".sstack/";

/// Variables with which git points its child processes at one repository. Any git run from inside
/// a hook inherits them, and they must go, because they are wrong anywhere but that repository,
/// and always wrong in a worktree.
pub(crate) const REPOSITORY_ENV: [&str; 3] = ["GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE"];

/// A git command that failed, with what it said.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct GitError(pub String);

/// Returns where the Session with id `id` has its worktree, which need not exist.
#[must_use]
pub fn path(main: &Path, id: &str) -> PathBuf {
    main.join(DATA_DIR).join("worktrees").join(id)
}

/// Returns the name of the branch a worktree Session works on.
#[must_use]
pub fn branch(id: &str) -> String {
    format!("session/{id}")
}

/// Returns the file that remembers which commands the person always allows in `main`.
#[must_use]
pub fn approved_commands_path(main: &Path) -> PathBuf {
    main.join(DATA_DIR).join("approved-commands")
}

/// Gives the Session with id `id` a worktree on a new branch from what `main` has checked out,
/// returning where it is.
///
/// # Errors
/// Returns an error if `main` is not a git repository with a commit to branch from, or if the
/// branch already exists.
pub fn create(main: &Path, id: &str) -> Result<PathBuf, GitError> {
    exclude_data_dir(main)?;
    let path = path(main, id);
    let target = path.to_string_lossy();
    git(
        main,
        &["worktree", "add", "-q", "-b", &branch(id), &target, "HEAD"],
    )?;
    Ok(path)
}

/// Keeps worktrees out of `git status` and out of the model's sight in the main checkout, without
/// changing any tracked file.
fn exclude_data_dir(main: &Path) -> Result<(), GitError> {
    let common = PathBuf::from(git(main, &["rev-parse", "--git-common-dir"])?.trim());
    let exclude = main.join(common).join("info").join("exclude");
    let existing = fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|line| line == EXCLUDE_LINE) {
        return Ok(());
    }
    append_line(&exclude, EXCLUDE_LINE).map_err(|error| GitError(error.to_string()))
}

/// Adds `line` to the end of the file at `path`, creating it and its directory if needed.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

/// Returns why removing the worktree of the Session with id `id` would lose work, if it would.
///
/// A Session with no worktree has nothing to lose.
///
/// # Errors
/// Returns an error if git cannot tell.
pub fn unfinished_work(main: &Path, id: &str) -> Result<Option<String>, GitError> {
    let worktree = path(main, id);
    if !worktree.exists() {
        return Ok(None);
    }
    if !git(&worktree, &["status", "--porcelain"])?
        .trim()
        .is_empty()
    {
        return Ok(Some("it has uncommitted changes".to_owned()));
    }
    let range = format!("HEAD..{}", branch(id));
    let unlanded = git(main, &["rev-list", "--count", &range])?;
    Ok((unlanded.trim() != "0").then(|| format!("it has {} unlanded commits", unlanded.trim())))
}

/// Deletes the worktree and branch of the Session with id `id`, if it has them, and any work in
/// them.
///
/// # Errors
/// Returns an error if git refuses.
pub fn remove(main: &Path, id: &str) -> Result<(), GitError> {
    let worktree = path(main, id);
    if !worktree.exists() {
        return Ok(());
    }
    let target = worktree.to_string_lossy();
    git(main, &["worktree", "remove", "--force", &target])?;
    git(main, &["branch", "-q", "-D", &branch(id)])?;
    Ok(())
}

/// Runs git in `directory`, returning what it printed.
fn git(directory: &Path, args: &[&str]) -> Result<String, GitError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(directory).args(args);
    for name in REPOSITORY_ENV {
        command.env_remove(name);
    }
    let output = command
        .output()
        .map_err(|error| GitError(format!("failed to start git: {error}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let said = String::from_utf8_lossy(&output.stderr);
        Err(GitError(said.trim().to_owned()))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    use crate::tools::test_support::git;

    /// Makes `root` a repository with one commit, with identity set so that commits work anywhere.
    pub fn repository(root: &Path) {
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["commit", "-q", "--allow-empty", "-m", "first"]);
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::repository;
    use super::*;
    use crate::tools::test_support::git as run;

    #[test]
    fn a_worktree_is_a_branch_from_the_main_checkout_that_git_status_ignores() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());

        let worktree = create(dir.path(), "7").unwrap();

        assert_eq!(worktree, path(dir.path(), "7"));
        assert!(worktree.starts_with(dir.path().join(".sstack")));
        let current = git(&worktree, &["branch", "--show-current"]).unwrap();
        assert_eq!(current.trim(), "session/7");
        assert_eq!(git(dir.path(), &["status", "--porcelain"]).unwrap(), "");
    }

    #[test]
    fn the_exclusion_is_added_only_once() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        create(dir.path(), "1").unwrap();
        create(dir.path(), "2").unwrap();
        let exclude = fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches(EXCLUDE_LINE).count(), 1);
    }

    #[test]
    fn a_directory_that_is_not_a_repository_has_no_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        assert!(create(dir.path(), "1").is_err());
    }

    #[test]
    fn work_that_would_be_lost_is_named() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        assert_eq!(unfinished_work(dir.path(), "1"), Ok(None));
        let worktree = create(dir.path(), "1").unwrap();
        assert_eq!(unfinished_work(dir.path(), "1"), Ok(None));

        fs::write(worktree.join("new.txt"), "").unwrap();
        let dirty = unfinished_work(dir.path(), "1").unwrap().unwrap();
        assert!(dirty.contains("uncommitted"), "{dirty}");

        run(&worktree, &["add", "."]);
        run(&worktree, &["commit", "-q", "-m", "work"]);
        let unlanded = unfinished_work(dir.path(), "1").unwrap().unwrap();
        assert!(unlanded.contains("1 unlanded"), "{unlanded}");
    }

    #[test]
    fn removing_deletes_the_worktree_and_its_branch_even_with_work_in_it() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let worktree = create(dir.path(), "1").unwrap();
        fs::write(worktree.join("new.txt"), "").unwrap();

        remove(dir.path(), "1").unwrap();

        assert!(!worktree.exists());
        assert!(git(dir.path(), &["rev-parse", "--verify", "session/1"]).is_err());
        assert_eq!(remove(dir.path(), "1"), Ok(()));
    }
}
