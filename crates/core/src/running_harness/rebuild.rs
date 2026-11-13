//! The work behind a Rebuild: building the Harness Source and relaunching into the result.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::workspace::{self, Checkout};

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

/// Returns the checkout of the Harness Source this code was built from, which every binary in it
/// shares. It may be a Session's worktree.
fn source_dir() -> PathBuf {
    let core = Path::new(env!("CARGO_MANIFEST_DIR"));
    core.ancestors().nth(2).unwrap_or(core).to_owned()
}

/// Returns the directory to build for a Rebuild of `checkout`, asked for from a Session whose
/// worktree belongs at `worktree`, as [`workspace::worktree_path`] gives, whether or not it has
/// one.
///
/// # Errors
/// Fails for [`Checkout::Worktree`] if the Session's project is not the Harness Source this code
/// was built from, or if the Session has no worktree.
pub fn source_to_build(checkout: Checkout, worktree: &Path) -> Result<PathBuf, String> {
    source_from(&source_dir(), checkout, worktree)
}

/// Returns what [`source_to_build`] does, for code built from the checkout `built_from`.
fn source_from(built_from: &Path, checkout: Checkout, worktree: &Path) -> Result<PathBuf, String> {
    let home = workspace::main_checkout_of(built_from).unwrap_or(built_from);
    match checkout {
        Checkout::Main => Ok(home.to_owned()),
        Checkout::Worktree => worktree_source(home, worktree),
    }
}

/// Returns `worktree`, a Session's worktree, if it is one of the Harness Source whose main
/// checkout is `home`.
fn worktree_source(home: &Path, worktree: &Path) -> Result<PathBuf, String> {
    let main = workspace::main_checkout_of(worktree);
    if !main.is_some_and(|main| same_directory(home, main)) {
        return Err(format!(
            "cannot build this Session's worktree: this project is not the Harness Source at {}",
            home.display()
        ));
    }
    if !worktree.is_dir() {
        return Err(
            "cannot build this Session's worktree: it works in the main checkout".to_owned(),
        );
    }
    Ok(worktree.to_owned())
}

/// Returns whether `a` and `b` name the same existing directory, however each is spelled.
fn same_directory(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
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

/// The cargo profile a Rebuild builds with, whatever the running binary was built with. It is
/// the one tests build with, so a Rebuild after a test run can reuse much of its work.
const PROFILE: &str = "dev";

/// The directory under `target` where cargo puts what [`PROFILE`] builds.
const PROFILE_DIR: &str = "debug";

/// Builds `binary` from the checkout of the Harness Source at `source`, which
/// [`source_to_build`] gives, with the `dev` profile even if the running code is a release build.
/// Returns where the new binary is, or what went wrong. Each checkout builds into its own `target`
/// directory.
///
/// # Errors
/// Returns the end of cargo's output if the build fails, or why cargo could not start.
pub async fn build_harness(binary: Binary, source: PathBuf) -> Result<PathBuf, String> {
    run_build(build_command(&source, binary.package, PROFILE)).await?;
    Ok(binary_path(&source, PROFILE_DIR, binary.name))
}

/// Replaces this process with `binary`. Only returns if that fails.
#[must_use]
pub fn relaunch_into(binary: &Path, args: &[String]) -> io::Error {
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
    fn this_test_binary_is_built_from_the_harness_source() {
        assert!(source_dir().join("Cargo.toml").exists());
        assert!(source_dir().join("crates/core").is_dir());
    }

    #[test]
    fn the_main_checkout_is_built_even_by_code_built_in_a_worktree() {
        let home = tempfile::tempdir().unwrap();
        let built_in_worktree = workspace::worktree_path(home.path(), "7");
        let elsewhere = Path::new("/other");
        for built_from in [home.path(), &built_in_worktree] {
            let built = source_from(built_from, Checkout::Main, elsewhere);
            assert_eq!(built.as_deref(), Ok(home.path()));
        }
    }

    #[test]
    fn a_sessions_worktree_is_built_only_if_it_is_one_of_the_harness_source() {
        let home = tempfile::tempdir().unwrap();
        let worktree = workspace::worktree_path(home.path(), "7");
        std::fs::create_dir_all(&worktree).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let build =
            |built_from: &Path, asking: &Path| source_from(built_from, Checkout::Worktree, asking);

        assert_eq!(build(home.path(), &worktree), Ok(worktree.clone()));
        assert_eq!(build(&worktree, &worktree), Ok(worktree.clone()));
        let none = build(home.path(), &workspace::worktree_path(home.path(), "8")).unwrap_err();
        assert!(none.contains("main checkout"), "{none}");
        let other = workspace::worktree_path(elsewhere.path(), "7");
        std::fs::create_dir_all(&other).unwrap();
        let other_project = build(home.path(), &other).unwrap_err();
        assert!(
            other_project.contains("not the Harness Source"),
            "{other_project}"
        );
    }

    #[test]
    fn relaunching_a_missing_binary_reports_the_failure() {
        let error = relaunch_into(Path::new("/definitely/not/here"), &[]);
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
