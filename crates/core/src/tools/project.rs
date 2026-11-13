//! Which files the model may touch.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use super::ToolError;
use crate::workspace::{REPOSITORY_ENV, is_protected};

/// The directory a Session works in, which limits what the model can touch.
#[derive(Debug)]
pub struct Project {
    /// The project's root directory, with symlinks resolved.
    root: PathBuf,
    /// Directories outside the project whose files the model may read but not change, with
    /// symlinks resolved.
    readable: Vec<PathBuf>,
}

impl Project {
    /// Creates a project rooted at `root`.
    ///
    /// # Errors
    /// Returns an error if `root` does not exist.
    pub fn new(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
            readable: Vec::new(),
        })
    }

    /// Also lets the model read, but not change, the files in `directories`, such as those of
    /// the Skills it is offered. Directories that do not exist are left out.
    #[must_use]
    pub fn with_readable(mut self, directories: impl IntoIterator<Item = PathBuf>) -> Self {
        let real = directories
            .into_iter()
            .filter_map(|d| d.canonicalize().ok());
        self.readable.extend(real);
        self
    }

    /// Returns the project's root directory.
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
        let denied = || ToolError::Failed(format!("access denied: {path}"));
        let resolved = self.canonical(path).ok_or_else(denied)?;
        let visible = resolved.starts_with(&self.root)
            && !resolved.strip_prefix(&self.root).is_ok_and(is_protected)
            && !self.is_git_ignored(&resolved);
        visible.then_some(resolved).ok_or_else(denied)
    }

    /// Resolves a model-supplied path to a file the model may read, which is any file it may
    /// access plus those in the directories given to [`Project::with_readable`]. A path into
    /// one of those directories must be absolute.
    ///
    /// # Errors
    /// Returns an error if the model may not read the path.
    pub fn resolve_for_reading(&self, path: &str) -> Result<PathBuf, ToolError> {
        self.resolve(path).or_else(|denied| {
            let resolved = self.canonical(path).ok_or(denied)?;
            let readable = (self.readable.iter()).any(|d| {
                resolved
                    .strip_prefix(d)
                    .is_ok_and(|inside| !is_protected(inside))
            });
            if readable {
                Ok(resolved)
            } else {
                Err(ToolError::Failed(format!("access denied: {path}")))
            }
        })
    }

    /// Resolves `path` against the root, following symlinks through the parts that exist.
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

    /// Returns whether git ignores `path`.
    ///
    /// Tracked files stay visible, as they do in git. Failing to ask counts as ignored, so a
    /// broken git never exposes a file.
    fn is_git_ignored(&self, path: &Path) -> bool {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.root)
            .args(["check-ignore", "-q", "--"])
            .arg(path);
        for name in REPOSITORY_ENV {
            command.env_remove(name);
        }
        command
            .output()
            .map_or(true, |output| output.status.success())
    }
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
    fn a_root_inside_the_data_directory_still_shows_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".sstack/worktrees/1");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), "").unwrap();
        let project = Project::new(&root).unwrap();
        assert!(project.resolve("a.txt").is_ok());
        assert!(project.resolve(".env").is_err());
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
    fn readable_directories_can_be_read_but_not_otherwise_accessed() {
        let dir = tempfile::tempdir().unwrap();
        let skill = tempfile::tempdir().unwrap();
        fs::write(skill.path().join("notes.md"), "").unwrap();
        fs::write(skill.path().join(".env"), "").unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("secret.md"), "").unwrap();
        let project = Project::new(dir.path()).unwrap();
        let project = project.with_readable([skill.path().to_owned(), "/not/here".into()]);
        let inside = |name: &str| skill.path().join(name).display().to_string();

        assert!(project.resolve_for_reading(&inside("notes.md")).is_ok());
        assert!(project.resolve(&inside("notes.md")).is_err());
        assert!(project.resolve_for_reading(&inside(".env")).is_err());
        let outside = elsewhere.path().join("secret.md").display().to_string();
        assert!(project.resolve_for_reading(&outside).is_err());
    }

    #[test]
    fn a_missing_root_is_an_error() {
        assert!(Project::new(Path::new("/definitely/not/here")).is_err());
    }
}
