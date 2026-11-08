//! Driving a container runtime from its command line.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

/// The label every Sandbox's container carries, holding its Session's id.
pub(super) const SESSION_LABEL: &str = "sstack.session";
/// The label holding the process id of the Harness that started the container.
pub(super) const HARNESS_LABEL: &str = "sstack.harness-pid";
/// The label holding the tag of the image the container was started from.
pub(super) const IMAGE_LABEL: &str = "sstack.image";

/// How long a runtime may take to say whether it works before it counts as missing.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs a Sandbox's containers, whichever program provides them.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait ContainerRuntime: Send + Sync {
    /// Returns the runtime's name, as the person would type it.
    fn name(&self) -> &'static str;

    /// Returns whether the runtime is installed and ready to run containers.
    async fn available(&self) -> bool;

    /// Returns whether the image tagged `tag` has already been built.
    async fn has_image(&self, tag: &str) -> bool;

    /// Builds the `Containerfile` in `context` and tags the result `tag`.
    ///
    /// # Errors
    /// Returns what the runtime said if the build fails.
    async fn build_image(&self, tag: &str, context: &Path) -> Result<(), RuntimeError>;

    /// Describes the container named `name`, or returns nothing if there is none.
    async fn inspect(&self, name: &str) -> Option<Inspected>;

    /// Returns the names of every container any Harness has started, running or not.
    async fn sandboxes(&self) -> Vec<String>;

    /// Starts a container as `spec` describes.
    ///
    /// # Errors
    /// Returns what the runtime said if the container does not start.
    async fn start(&self, spec: &ContainerSpec) -> Result<(), RuntimeError>;

    /// Runs `argv` in the container named `name`, from the Workspace, and waits for it.
    ///
    /// # Errors
    /// Returns an error if the runtime itself cannot be started.
    async fn exec(&self, name: &str, argv: &[String]) -> io::Result<Output>;

    /// Stops and deletes the container named `name`, if there is one, and waits until it has
    /// gone.
    async fn remove(&self, name: &str);

    /// Starts deleting the container named `name` without waiting, for callers that cannot
    /// wait, such as `Drop`.
    fn remove_detached(&self, name: &str);
}

/// What a runtime reports about one container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspected {
    /// Whether the container is running now.
    pub running: bool,
    /// The tag of the image the container was started from, or empty if it has no such label.
    pub image: String,
    /// The process id of the Harness that started it, if it says.
    pub harness: Option<i32>,
}

/// A failure a runtime reported, in its own words.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RuntimeError(pub String);

/// Everything that decides how a Sandbox's container is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSpec {
    /// The container's name, unique to its Session.
    pub name: String,
    /// The tag of the image to start.
    pub image: String,
    /// The Workspace, mounted read-write at the same path it has on the host.
    pub workspace: PathBuf,
    /// The Protected Paths inside the Workspace, covered so the container cannot see them.
    pub masks: Vec<Mask>,
    /// Labels identifying the container, as name and value.
    pub labels: Vec<(String, String)>,
    /// The user and group id commands run as, so that files they write belong to the person.
    pub user: (u32, u32),
}

/// A Protected Path, covered by something empty and read-only of the same kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mask {
    /// A file, which reads as empty.
    File(PathBuf),
    /// A directory, which reads as empty.
    Directory(PathBuf),
}

/// The program a container waits in, doing nothing but collecting the processes that commands
/// leave behind, and quitting at once when stopped.
const IDLE: &str = "trap 'exit 0' TERM; while :; do sleep 3600 & wait; done";

/// Drives any runtime whose command line takes Docker's arguments, such as Podman, Finch,
/// nerdctl and Docker itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockerCli {
    /// The program to run, such as `podman`.
    program: &'static str,
}

impl DockerCli {
    /// Creates a driver for the runtime whose program is `program`.
    #[must_use]
    pub const fn new(program: &'static str) -> Self {
        Self { program }
    }

    /// Starts a command for this runtime with `args`, with no input.
    fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut command = Command::new(self.program);
        command
            .args(args.into_iter().map(Into::into))
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// Runs this runtime with `args`, returning what it printed if it succeeded.
    async fn output<I, S>(&self, args: I) -> Result<String, RuntimeError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.command(args).output().await;
        let output = output.map_err(|e| RuntimeError(format!("{}: {e}", self.program)))?;
        succeeded(&output)
    }
}

/// Returns what a finished runtime command printed, or what it complained about if it failed.
fn succeeded(output: &Output) -> Result<String, RuntimeError> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let said = String::from_utf8_lossy(&output.stderr);
        Err(RuntimeError(said.trim().to_owned()))
    }
}

/// The format `inspect` asks for, one field per line, which Docker, Podman and nerdctl all
/// understand.
const INSPECT_FORMAT: &str = concat!(
    "{{.State.Running}}\n",
    "{{index .Config.Labels \"sstack.image\"}}\n",
    "{{index .Config.Labels \"sstack.harness-pid\"}}"
);

/// Reads the answer to an `inspect` that used [`INSPECT_FORMAT`].
fn parse_inspected(text: &str) -> Inspected {
    let mut lines = text.lines().map(str::trim);
    let running = lines.next() == Some("true");
    let image = lines.next().unwrap_or_default().to_owned();
    let harness = lines.next().and_then(|pid| pid.parse().ok());
    Inspected {
        running,
        image,
        harness,
    }
}

/// Returns the arguments that start the container `spec` describes, detached and with no
/// network.
fn start_args(program: &str, spec: &ContainerSpec) -> Vec<String> {
    let workspace = spec.workspace.display().to_string();
    let mut args = vec![
        "run".to_owned(),
        "--detach".to_owned(),
        "--name".to_owned(),
        spec.name.clone(),
        "--network".to_owned(),
        "none".to_owned(),
        "--volume".to_owned(),
        format!("{workspace}:{workspace}"),
    ];
    args.extend(spec.masks.iter().flat_map(|mask| mask_args(program, mask)));
    for (name, value) in &spec.labels {
        args.extend(["--label".to_owned(), format!("{name}={value}")]);
    }
    args.extend(user_args(program, spec.user));
    args.extend(["--env".to_owned(), "HOME=/tmp".to_owned()]);
    args.extend(["--workdir".to_owned(), workspace]);
    args.extend([spec.image.clone(), "bash".to_owned(), "-c".to_owned()]);
    args.push(IDLE.to_owned());
    args
}

/// Returns the arguments that cover one Protected Path.
///
/// Podman fills a new tmpfs with whatever it covers unless told `notmpcopyup`, which would
/// copy the very directory it is meant to hide. Other runtimes take no such option.
fn mask_args(program: &str, mask: &Mask) -> [String; 2] {
    match mask {
        Mask::File(path) => [
            "--volume".to_owned(),
            format!("/dev/null:{}:ro", path.display()),
        ],
        Mask::Directory(path) => {
            let empty = if program == "podman" {
                ",notmpcopyup"
            } else {
                ""
            };
            [
                "--tmpfs".to_owned(),
                format!("{}:ro{empty}", path.display()),
            ]
        }
    }
}

/// Returns the arguments that make files written in the container belong to `user`, the
/// person's own ids.
///
/// Rootless Podman already maps the container's root to the person, so it needs none. Asking
/// it for `--userns=keep-id` instead makes some containers fail to start when several start at
/// once. Other runtimes run as root unless told the ids.
fn user_args(program: &str, (uid, gid): (u32, u32)) -> Vec<String> {
    if program == "podman" {
        Vec::new()
    } else {
        vec!["--user".to_owned(), format!("{uid}:{gid}")]
    }
}

/// Returns the arguments that run `command` in the container `name`, from the directory it
/// was started in.
fn exec_args(name: &str, command: &[String]) -> Vec<String> {
    let mut args = vec!["exec".to_owned(), name.to_owned()];
    args.extend(command.iter().cloned());
    args
}

#[async_trait]
impl ContainerRuntime for DockerCli {
    fn name(&self) -> &'static str {
        self.program
    }

    async fn available(&self) -> bool {
        let probe = self.output(["info"]);
        matches!(tokio::time::timeout(PROBE_TIMEOUT, probe).await, Ok(Ok(_)))
    }

    async fn has_image(&self, tag: &str) -> bool {
        self.output(["image", "inspect", tag]).await.is_ok()
    }

    async fn build_image(&self, tag: &str, context: &Path) -> Result<(), RuntimeError> {
        let args = ["build".into(), "--tag".into(), tag.into(), context.into()];
        self.output::<[OsString; 4], _>(args).await.map(drop)
    }

    async fn inspect(&self, name: &str) -> Option<Inspected> {
        let args = ["container", "inspect", "--format", INSPECT_FORMAT, name];
        self.output(args)
            .await
            .ok()
            .map(|text| parse_inspected(&text))
    }

    async fn sandboxes(&self) -> Vec<String> {
        let filter = format!("label={SESSION_LABEL}");
        let args = ["ps", "--all", "--filter", &filter, "--format", "{{.Names}}"];
        let listed = self.output(args).await.unwrap_or_default();
        listed.lines().map(str::to_owned).collect()
    }

    async fn start(&self, spec: &ContainerSpec) -> Result<(), RuntimeError> {
        self.output(start_args(self.program, spec)).await.map(drop)
    }

    async fn exec(&self, name: &str, argv: &[String]) -> io::Result<Output> {
        self.command(exec_args(name, argv)).output().await
    }

    async fn remove(&self, name: &str) {
        let _ = self.output(["rm", "--force", name]).await;
    }

    fn remove_detached(&self, name: &str) {
        let _ = std::process::Command::new(self.program)
            .args(["rm", "--force", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn spec() -> ContainerSpec {
        ContainerSpec {
            name: "sstack-7".into(),
            image: "sstack-sandbox:abc".into(),
            workspace: "/w".into(),
            masks: vec![
                Mask::File("/w/.git".into()),
                Mask::Directory("/w/.sstack".into()),
            ],
            labels: vec![("sstack.session".into(), "7".into())],
            user: (501, 20),
        }
    }

    #[test]
    fn a_container_has_no_network_and_sees_the_workspace_at_its_own_path() {
        let args = start_args("docker", &spec()).join(" ");
        assert!(args.starts_with("run --detach --name sstack-7 --network none"));
        assert!(args.contains("--volume /w:/w"));
        assert!(args.contains("--workdir /w sstack-sandbox:abc bash -c"));
    }

    #[test]
    fn protected_paths_are_covered_by_empty_read_only_mounts() {
        let args = start_args("docker", &spec()).join(" ");
        assert!(args.contains("--volume /dev/null:/w/.git:ro"));
        assert!(args.contains("--tmpfs /w/.sstack:ro "));
        assert!(args.contains("--label sstack.session=7"));
        let podman = start_args("podman", &spec()).join(" ");
        assert!(podman.contains("--tmpfs /w/.sstack:ro,notmpcopyup"));
    }

    #[test]
    fn commands_run_as_the_person_on_every_runtime() {
        let docker = start_args("docker", &spec()).join(" ");
        let podman = start_args("podman", &spec()).join(" ");
        assert!(docker.contains("--user 501:20"));
        assert!(!podman.contains("--user"));
    }

    #[test]
    fn a_command_runs_in_the_named_container_as_given() {
        let command = ["bash".to_owned(), "-c".to_owned(), "ls; pwd".to_owned()];
        let args = exec_args("sstack-7", &command);
        assert_eq!(args, ["exec", "sstack-7", "bash", "-c", "ls; pwd"]);
    }

    #[test]
    fn inspection_reads_state_and_labels() {
        let inspected = parse_inspected("true\nsstack-sandbox:abc\n42\n");
        assert_eq!(
            inspected,
            Inspected {
                running: true,
                image: "sstack-sandbox:abc".into(),
                harness: Some(42),
            }
        );
        let bare = parse_inspected("false\n\n<no value>\n");
        assert!(!bare.running);
        assert_eq!(bare.harness, None);
    }

    #[test]
    fn a_failed_command_reports_what_it_said() {
        let output = |code, stdout: &str, stderr: &str| Output {
            status: ExitStatus::from_raw(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        assert_eq!(succeeded(&output(0, "id\n", "")), Ok("id\n".to_owned()));
        let failed = succeeded(&output(256, "", " no such image \n"));
        assert_eq!(failed, Err(RuntimeError("no such image".to_owned())));
    }
}
