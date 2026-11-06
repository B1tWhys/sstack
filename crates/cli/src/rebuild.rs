//! Building the Harness Source and relaunching into the result.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace this binary was built from.
fn source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn build_command(source: &Path, profile: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .args(["build", "--package", "sstack", "--profile", profile])
        .current_dir(source);
    command
}

fn binary_path(source: &Path, profile_dir: &str) -> PathBuf {
    source.join("target").join(profile_dir).join("sstack")
}

/// A failed build must leave the Session running, so this reports it rather than exiting.
fn succeeded(command: &mut Command) -> bool {
    command.status().is_ok_and(|status| status.success())
}

fn profile() -> (&'static str, &'static str) {
    if cfg!(debug_assertions) {
        ("dev", "debug")
    } else {
        ("release", "release")
    }
}

/// Builds the source, returning the new binary if that worked.
pub fn build() -> Option<PathBuf> {
    let (profile, directory) = profile();
    let source = source_dir();
    succeeded(&mut build_command(&source, profile)).then(|| binary_path(&source, directory))
}

/// Replaces this process with `binary`. Only returns if that fails.
pub fn relaunch(binary: &Path, args: &[String]) -> io::Error {
    Command::new(binary).args(args).exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_targets_the_harness_binary() {
        let command = build_command(Path::new("/src"), "dev");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["build", "--package", "sstack", "--profile", "dev"]);
        assert_eq!(command.get_current_dir(), Some(Path::new("/src")));
    }

    #[test]
    fn the_binary_is_found_under_the_profile_directory() {
        let path = binary_path(Path::new("/src"), "debug");
        assert_eq!(path, Path::new("/src/target/debug/sstack"));
    }

    #[test]
    fn a_failed_build_does_not_count() {
        assert!(succeeded(&mut Command::new("true")));
        assert!(!succeeded(&mut Command::new("false")));
        assert!(!succeeded(&mut Command::new("/definitely/not/here")));
    }

    #[test]
    fn this_test_binary_uses_the_dev_profile() {
        assert_eq!(profile(), ("dev", "debug"));
        assert!(source_dir().join("Cargo.toml").exists());
    }

    #[test]
    fn relaunching_a_missing_binary_reports_the_failure() {
        let error = relaunch(Path::new("/definitely/not/here"), &[]);
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
