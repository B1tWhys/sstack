//! Protected Paths: paths in the Workspace that the model can never see or change, by any tool or
//! from the Sandbox.

use std::ffi::OsStr;
use std::path::{Component, Path};
use std::sync::LazyLock;

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::worktree::DATA_DIR;

/// The directories that are Protected Paths wherever they are, with everything in them.
pub const PROTECTED_DIRECTORIES: [&str; 2] = [".git", DATA_DIR];

/// The names of the files that are Protected Paths wherever they are, as globs.
pub const PROTECTED_FILES: [&str; 2] = [".env", ".env.*"];

/// [`PROTECTED_FILES`], ready to match names against.
static PROTECTED_FILE_GLOBS: LazyLock<GlobSet> = LazyLock::new(|| {
    let mut globs = GlobSetBuilder::new();
    for pattern in PROTECTED_FILES {
        globs.add(Glob::new(pattern).expect("every protected file pattern is a valid glob"));
    }
    globs
        .build()
        .expect("the protected file patterns build into a set")
});

/// Returns whether `path`, relative to the directory that holds it, such as the Workspace, is a
/// Protected Path or inside one.
#[must_use]
pub fn is_protected(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => is_protected_name(name),
        _ => false,
    })
}

/// Returns whether a path with `name` as one of its parts is protected.
fn is_protected_name(name: &OsStr) -> bool {
    PROTECTED_DIRECTORIES
        .iter()
        .any(|directory| name == *directory)
        || PROTECTED_FILE_GLOBS.is_match(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_the_data_directory_and_env_files_are_protected_wherever_they_are() {
        for path in [
            ".git",
            "sub/.git/config",
            ".sstack/x",
            ".env",
            "a/.env.local",
        ] {
            assert!(is_protected(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn other_paths_are_not_protected() {
        for path in ["src/main.rs", ".gitignore", "env", ".environment", ""] {
            assert!(!is_protected(Path::new(path)), "{path}");
        }
    }
}
