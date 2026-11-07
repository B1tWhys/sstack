//! Building the Harness Source and relaunching into the result.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Returns the Harness Source this binary was built from.
fn source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the command that compiles the harness binary from `source`.
fn build_command(source: &Path, profile: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .args(["build", "--package", "sstack", "--profile", profile])
        .current_dir(source);
    command
}

/// Returns where cargo puts the binary for a profile's output directory.
fn binary_path(source: &Path, profile_dir: &str) -> PathBuf {
    source.join("target").join(profile_dir).join("sstack")
}

/// Returns whether `command` ran and succeeded. Failing to start counts as not succeeding.
fn succeeded(command: &mut Command) -> bool {
    command.status().is_ok_and(|status| status.success())
}

/// Returns the cargo profile this binary was built with, and its output directory.
fn profile() -> (&'static str, &'static str) {
    if cfg!(debug_assertions) {
        ("dev", "debug")
    } else {
        ("release", "release")
    }
}

/// Builds the Harness Source, returning the new binary if that worked.
pub fn build() -> Option<PathBuf> {
    let (profile, directory) = profile();
    let source = source_dir();
    succeeded(&mut build_command(&source, profile)).then(|| binary_path(&source, directory))
}

/// Replaces this process with `binary`. Only returns if that fails.
pub fn relaunch(binary: &Path, args: &[String]) -> io::Error {
    Command::new(binary).args(args).exec()
}

/// Rebuilds the Harness Source and relaunches into it, passing the new binary `relaunch_args`.
///
/// Only returns if that could not be done, saying why. Build output goes straight to the
/// terminal, so the caller should hand the terminal back first.
pub fn rebuild(relaunch_args: &[String]) -> String {
    Rebuilder { build, relaunch }.run(relaunch_args)
}

/// The two steps of a Rebuild, kept apart so that the decision between them can be tested.
struct Rebuilder<B, R> {
    /// Builds the Harness Source, returning the new binary if that worked.
    build: B,
    /// Replaces this process with a binary, returning only if that fails.
    relaunch: R,
}

impl<B, R> Rebuilder<B, R>
where
    B: FnOnce() -> Option<PathBuf>,
    R: FnOnce(&Path, &[String]) -> io::Error,
{
    /// Replaces the Running Harness with a fresh build, passing it `relaunch_args`.
    ///
    /// Only returns if that could not be done, saying why.
    fn run(self, relaunch_args: &[String]) -> String {
        match (self.build)() {
            Some(binary) => (self.relaunch)(&binary, relaunch_args).to_string(),
            None => "the build did not succeed".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_build_does_not_relaunch_and_says_so() {
        let rebuilder = Rebuilder {
            build: || None,
            relaunch: |_: &Path, _: &[String]| panic!("a failed build must not relaunch"),
        };
        assert_eq!(rebuilder.run(&[]), "the build did not succeed");
    }

    #[test]
    fn a_successful_build_relaunches_with_the_given_arguments() {
        let mut launched = None;
        let rebuilder = Rebuilder {
            build: || Some(PathBuf::from("/bin/sstack")),
            relaunch: |binary: &Path, arguments: &[String]| {
                launched = Some((binary.to_owned(), arguments.to_vec()));
                io::Error::other("exec failed")
            },
        };
        let failure = rebuilder.run(&["--resume".to_owned()]);
        let (binary, arguments) = launched.unwrap();
        assert_eq!(binary, Path::new("/bin/sstack"));
        assert_eq!(arguments, ["--resume"]);
        assert_eq!(failure, "exec failed");
    }

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
