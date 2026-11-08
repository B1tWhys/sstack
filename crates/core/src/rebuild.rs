//! The work behind a Rebuild: building the Harness Source and relaunching into the result.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How many lines of a failed build to show the person.
const MAX_ERROR_LINES: usize = 20;

/// One of the binaries in the Harness Source, which a Rebuild builds and relaunches into. Each
/// interface passes its own, so that a Rebuild relaunches the interface the person is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binary {
    /// The cargo package the binary belongs to, such as `sstack-cli`.
    pub package: &'static str,
    /// The name of the binary, which is also its file name in cargo's output directory, such as
    /// `sstack`.
    pub name: &'static str,
}

/// Returns the Harness Source this code was built from, which every binary in it shares.
fn source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the command that compiles `package` from `source`.
fn build_command(source: &Path, package: &str, profile: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .args(["build", "--package", package, "--profile", profile])
        .current_dir(source);
    command
}

/// Returns where cargo puts the binary called `name` for a profile's output directory.
fn binary_path(source: &Path, profile_dir: &str, name: &str) -> PathBuf {
    source.join("target").join(profile_dir).join(name)
}

/// Runs `command` to its end, returning what went wrong if it fails. Stops the build if dropped,
/// so a Rebuild that is abandoned leaves nothing running.
async fn run_build(command: Command) -> Result<(), String> {
    let mut command = tokio::process::Command::from(command);
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("could not start the build: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Err(format!("the build did not succeed:\n{}", tail(&said)))
}

/// Keeps the end of build output, where the failure is.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(MAX_ERROR_LINES)..].join("\n")
}

/// Returns the cargo profile this binary was built with, and its output directory.
fn profile() -> (&'static str, &'static str) {
    if cfg!(debug_assertions) {
        ("dev", "debug")
    } else {
        ("release", "release")
    }
}

/// Builds `binary` from the Harness Source with the profile the running code was built with,
/// returning where the new binary is, or what went wrong.
///
/// # Errors
/// Returns the end of cargo's output if the build fails, or why cargo could not start.
pub async fn build(binary: Binary) -> Result<PathBuf, String> {
    let (profile, directory) = profile();
    let source = source_dir();
    run_build(build_command(&source, binary.package, profile)).await?;
    Ok(binary_path(&source, directory, binary.name))
}

/// Replaces this process with `binary`. Only returns if that fails.
#[must_use]
pub fn relaunch(binary: &Path, args: &[String]) -> io::Error {
    Command::new(binary).args(args).exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_targets_the_harness_binary() {
        let command = build_command(Path::new("/src"), "sstack-cli", "dev");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            ["build", "--package", "sstack-cli", "--profile", "dev"]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/src")));
    }

    #[test]
    fn the_binary_is_found_under_the_profile_directory() {
        let path = binary_path(Path::new("/src"), "debug", "sstack");
        assert_eq!(path, Path::new("/src/target/debug/sstack"));
    }

    #[tokio::test]
    async fn a_failed_build_says_what_went_wrong() {
        assert_eq!(run_build(Command::new("true")).await, Ok(()));

        let mut failing = Command::new("sh");
        failing.args(["-c", "echo broken >&2; exit 1"]);
        let failure = run_build(failing).await.unwrap_err();
        assert!(failure.ends_with("broken"), "{failure}");

        let missing = run_build(Command::new("/definitely/not/here")).await;
        assert!(missing.unwrap_err().contains("could not start"));
    }

    #[test]
    fn long_build_output_keeps_its_end() {
        let lines: Vec<String> = (0..MAX_ERROR_LINES + 5).map(|n| n.to_string()).collect();
        let kept = tail(&lines.join("\n"));
        assert_eq!(kept.lines().count(), MAX_ERROR_LINES);
        assert_eq!(kept.lines().next(), Some("5"));
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
