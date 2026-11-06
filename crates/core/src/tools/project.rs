//! Which files the model may touch.

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use super::ToolError;

/// The directory a Session works in.
#[derive(Debug)]
pub struct Project {
    root: PathBuf,
}

impl Project {
    /// # Errors
    /// Returns an error if `root` does not exist.
    pub fn new(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a model-supplied path to a file the model may access.
    ///
    /// Symlinks are followed before the checks, so a link cannot reach outside the project.
    ///
    /// # Errors
    /// Returns an error if the path is outside the project or hidden from the model.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, ToolError> {
        let denied = || ToolError(format!("access denied: {path}"));
        let resolved = self.canonical(path).ok_or_else(denied)?;
        let visible = resolved.starts_with(&self.root)
            && !is_protected(&resolved)
            && !self.is_git_ignored(&resolved);
        visible.then_some(resolved).ok_or_else(denied)
    }

    fn canonical(&self, path: &str) -> Option<PathBuf> {
        let joined = self.root.join(path);
        if joined.components().any(|c| c == Component::ParentDir) {
            return None;
        }
        let existing = joined.ancestors().find(|ancestor| ancestor.exists())?;
        let missing = joined.strip_prefix(existing).ok()?;
        let mut canonical = existing.canonicalize().ok()?;
        canonical.extend(missing);
        Some(canonical)
    }

    /// `check-ignore` reports nothing for tracked files, which stay visible as they do in git.
    /// Failing to ask counts as ignored, so a broken git never exposes a file.
    fn is_git_ignored(&self, path: &Path) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["check-ignore", "-q", "--"])
            .arg(path)
            .output()
            .map_or(true, |output| output.status.success())
    }
}

/// True for paths that stay hidden even when git does not ignore them, such as a `.env` file
/// in a directory that is not a repository.
pub(super) fn is_protected(path: &Path) -> bool {
    path.components().any(|c| is_protected_name(c.as_os_str()))
}

fn is_protected_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name == ".git" || name == ".env" || name.starts_with(".env.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{git, git_project};
    use std::fs;

    #[test]
    fn allows_project_files_including_ones_that_do_not_exist_yet() {
        let (dir, project) = git_project();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        assert!(project.resolve("a.txt").is_ok());
        assert!(project.resolve("new/dir/b.txt").is_ok());
    }

    #[test]
    fn refuses_paths_outside_the_project() {
        let (dir, project) = git_project();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(project.resolve("../escape").is_err());
        assert!(project.resolve("/etc/hosts").is_err());
        assert!(project.resolve("link/secret").is_err());
    }

    #[test]
    fn refuses_git_ignored_files_but_not_tracked_ones() {
        let (dir, project) = git_project();
        fs::create_dir(dir.path().join("ignored")).unwrap();
        fs::write(dir.path().join("ignored/a.txt"), "").unwrap();
        fs::write(dir.path().join("kept.log"), "").unwrap();
        fs::write(dir.path().join("other.log"), "").unwrap();
        git(dir.path(), &["add", "-f", "kept.log"]);

        assert!(project.resolve("ignored/a.txt").is_err());
        assert!(project.resolve("other.log").is_err());
        assert!(project.resolve("kept.log").is_ok());
    }

    #[test]
    fn env_files_and_git_internals_stay_hidden_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::new(dir.path()).unwrap();
        assert!(project.resolve("plain.txt").is_ok());
        assert!(project.resolve(".env").is_err());
        assert!(project.resolve("sub/.env.local").is_err());
        assert!(project.resolve(".git/config").is_err());
    }

    #[test]
    fn a_missing_root_is_an_error() {
        assert!(Project::new(Path::new("/definitely/not/here")).is_err());
    }
}
