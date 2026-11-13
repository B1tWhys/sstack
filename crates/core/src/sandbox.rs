//! A restricted environment for the model's shell commands, which can change nothing but the
//! Workspace and has no network, so they need nobody's approval. Cargo works offline in it,
//! from the crates the host has downloaded.
//!
//! Each Session's Sandbox is a long-lived container, started by its first command and removed
//! when the Session goes. Commands run in it one `exec` at a time. Which runtime runs the
//! containers is decided once for the whole Harness, by [`Containers`].

/// Tests against a real Podman.
#[cfg(all(test, feature = "podman-tests"))]
mod podman;
mod runtime;

use std::fmt::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::{Pid, getgid, getuid};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OnceCell};

use crate::worktree::{DATA_DIR, main_of};
#[cfg(test)]
pub(crate) use runtime::MockContainerRuntime;
pub use runtime::{
    CacheVolume, ContainerRuntime, ContainerSpec, DockerCli, Inspected, Mask, ReadOnly,
    RuntimeError,
};
use runtime::{HARNESS_LABEL, IMAGE_LABEL, SESSION_LABEL};

/// The programs that may run Sandboxes, in the order they are tried.
pub const RUNTIMES: [&str; 4] = [
    // "container", Apple's runtime, once it has an implementation of its own: its command line
    // does not take Docker's arguments.
    "podman", "finch", "nerdctl", "docker",
];

/// What the Sandbox image is built from.
const CONTAINERFILE: &str = include_str!("sandbox/Containerfile");

/// Where cargo keeps its files inside the container, which the [`CONTAINERFILE`] sets as
/// `CARGO_HOME`.
const CONTAINER_CARGO_HOME: &str = "/cargo";

/// The directories of cargo's home that hold downloaded crates, which the Sandbox shares from
/// the host.
const CRATE_DOWNLOADS: [&str; 2] = ["registry", "git"];

/// How much longer than its own limit a command may take before the Harness stops waiting,
/// which only happens if the runtime fails to stop it.
const GRACE: Duration = Duration::from_secs(15);

/// The exit status `timeout` reports for a command it stopped.
const TIMED_OUT_STATUS: i32 = 124;

/// Why the Sandbox did not run a command to its end.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// The Sandbox cannot run commands at all, for the reason given, which is meant for the
    /// person.
    #[error("{0}")]
    Unavailable(String),
    /// The command ran past its limit and was stopped.
    #[error("timed out after {0:?}")]
    TimedOut(Duration),
}

/// The container runtime and the image every Sandbox in the Harness uses, found
/// the first time one is needed.
///
/// That first time also removes the containers that Harnesses which have since gone left
/// behind.
pub struct Containers {
    /// The runtimes that may run Sandboxes, in the order they are tried.
    candidates: Vec<Arc<dyn ContainerRuntime>>,
    /// The runtime and image chosen, or why there are none, once decided.
    ready: OnceCell<Result<Ready, Failure>>,
}

/// A runtime that works, with the Sandbox image built.
struct Ready {
    /// The runtime every Sandbox uses.
    runtime: Arc<dyn ContainerRuntime>,
    /// The tag of the image every Sandbox starts from.
    image: String,
}

impl Containers {
    /// Uses the first of [`RUNTIMES`] that works on this machine.
    #[must_use]
    pub fn detect() -> Self {
        let candidates = RUNTIMES.iter().map(|program| {
            let runtime: Arc<dyn ContainerRuntime> = Arc::new(DockerCli::new(program));
            runtime
        });
        Self::new(candidates.collect())
    }

    /// Uses the first of `candidates` that works, in their order.
    #[must_use]
    pub fn new(candidates: Vec<Arc<dyn ContainerRuntime>>) -> Self {
        Self {
            candidates,
            ready: OnceCell::new(),
        }
    }

    /// Returns the runtime and image to use, deciding on them the first time.
    async fn ready(&self) -> Result<&Ready, Failure> {
        let ready = self.ready.get_or_init(|| self.prepare()).await;
        ready.as_ref().map_err(Clone::clone)
    }

    /// Chooses a runtime, clears out what gone Harnesses left behind, and builds the image if
    /// it is missing.
    async fn prepare(&self) -> Result<Ready, Failure> {
        let runtime = self.choose().await?;
        remove_orphans(runtime.as_ref()).await;
        let image = image_tag();
        ensure_image(runtime.as_ref(), &image).await?;
        Ok(Ready { runtime, image })
    }

    /// Returns the first candidate that works.
    async fn choose(&self) -> Result<Arc<dyn ContainerRuntime>, Failure> {
        for candidate in &self.candidates {
            if candidate.available().await {
                return Ok(candidate.clone());
            }
        }
        let tried: Vec<_> = self.candidates.iter().map(|c| c.name()).collect();
        Err(Failure::Unavailable(if tried.is_empty() {
            "the Sandbox is turned off".to_owned()
        } else {
            format!(
                "no container runtime is available (tried {})",
                tried.join(", ")
            )
        }))
    }
}

/// Removes every Sandbox container whose Harness has gone. One whose Harness is unknown is
/// left alone.
async fn remove_orphans(runtime: &dyn ContainerRuntime) {
    for name in runtime.sandboxes().await {
        let inspected = runtime.inspect(&name).await;
        let harness = inspected.and_then(|i| i.harness);
        if harness.is_some_and(|pid| !is_alive(pid)) {
            runtime.remove(&name).await;
        }
    }
}

/// Returns whether a process with id `pid` exists, even one this process may not signal.
fn is_alive(pid: i32) -> bool {
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}

/// Returns the tag of the image built from [`CONTAINERFILE`], which changes whenever it does.
fn image_tag() -> String {
    let digest = Sha256::digest(CONTAINERFILE.as_bytes());
    let hex = digest.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!("sstack-sandbox:{}", &hex[..12])
}

/// Builds the image tagged `tag` unless `runtime` already has it.
async fn ensure_image(runtime: &dyn ContainerRuntime, tag: &str) -> Result<(), Failure> {
    if runtime.has_image(tag).await {
        return Ok(());
    }
    let unbuilt = |error: String| Failure::Unavailable(format!("the image did not build: {error}"));
    let context = build_context(tag).map_err(|e| unbuilt(e.to_string()))?;
    let built = runtime.build_image(tag, &context).await;
    built.map_err(|e| unbuilt(e.0))
}

/// Writes [`CONTAINERFILE`] into a directory of its own for the image tagged `tag`, returning
/// the directory.
fn build_context(tag: &str) -> std::io::Result<PathBuf> {
    let directory = std::env::temp_dir().join(tag.replace(':', "-"));
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("Containerfile"), CONTAINERFILE)?;
    Ok(directory)
}

/// Returns the Protected Paths inside `workspace` that exist now, to be covered when the
/// Sandbox starts.
///
/// A `.env` file inside a directory git ignores is missed, since those directories are not
/// searched.
fn masks(workspace: &Path) -> Vec<Mask> {
    let fixed = [".git", DATA_DIR].map(|name| workspace.join(name));
    let fixed = fixed.into_iter().filter_map(mask);
    fixed.chain(env_files(workspace)).collect()
}

/// Returns how to cover `path`, whichever kind of thing it is, or nothing if it does not exist.
fn mask(path: PathBuf) -> Option<Mask> {
    let is_dir = path.symlink_metadata().ok()?.is_dir();
    Some(if is_dir {
        Mask::Directory(path)
    } else {
        Mask::File(path)
    })
}

/// Finds the `.env` files in `workspace`, including those git ignores, outside the directories
/// [`masks`] covers anyway.
fn env_files(workspace: &Path) -> Vec<Mask> {
    let mut globs = OverrideBuilder::new(workspace);
    let added = globs.add(".env").and_then(|g| g.add(".env.*"));
    let Ok(overrides) = added.and_then(|g| g.build()) else {
        return Vec::new();
    };
    let walk = WalkBuilder::new(workspace)
        .hidden(false)
        .overrides(overrides)
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some(".git" | DATA_DIR)))
        .build();
    walk.filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .map(|entry| Mask::File(entry.into_path()))
        .collect()
}

/// Returns the person's crate downloads in `cargo_home` on the host, shared read-only so that
/// cargo in the Sandbox can build offline.
///
/// Read-only matters: a Sandbox that could change a crate's source could plant a build script
/// that later runs on the host. Directories that do not exist are left out.
fn crate_downloads(cargo_home: &Path) -> Vec<ReadOnly> {
    CRATE_DOWNLOADS
        .iter()
        .filter_map(|name| {
            let host = cargo_home.join(name).canonicalize().ok()?;
            let container = Path::new(CONTAINER_CARGO_HOME).join(name);
            Some(ReadOnly { host, container })
        })
        .collect()
}

/// Returns the name of the volume that holds the build cache for the project `workspace` belongs
/// to. A Session worktree and the main checkout it came from name the same volume, so the cache
/// is shared only within one project.
fn cache_volume(workspace: &Path) -> String {
    let project = main_of(workspace).unwrap_or(workspace);
    let digest = Sha256::digest(project.as_os_str().as_encoded_bytes());
    let mut hex = String::new();
    for byte in &digest[..6] {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("sstack-cargo-{hex}")
}

/// Returns the command that runs `script` in the container, stopped after `limit`.
///
/// `timeout` stops the whole process group, so nothing a pipeline started outlives it.
fn bounded(script: &str, limit: Duration) -> Vec<String> {
    [
        "timeout",
        "--kill-after=5s",
        &format!("{}s", limit.as_secs()),
        "bash",
        "-c",
        script,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The container one Session's shell commands run in, which sees its Workspace, without its
/// Protected Paths, and the host's crate downloads, which it cannot change. It has no network.
///
/// The container starts with the first command and is removed when the Sandbox is dropped. A
/// command abandoned partway, as when its Turn is cancelled, takes the container with it,
/// since that is the only sure way to stop what the command started. The next command starts
/// a fresh one.
///
/// A container left running by this Session before, such as one started before a Rebuild, is
/// used again if it runs the current image.
pub struct Sandbox {
    /// Where the runtime and image come from.
    containers: Arc<Containers>,
    /// The id of the Session the Sandbox belongs to.
    session: String,
    /// The Workspace, with symlinks resolved.
    workspace: PathBuf,
    /// Held while the container starts, so that only one command at a time starts it.
    starting: Mutex<()>,
    /// Whether this Sandbox's container is believed to be running.
    started: AtomicBool,
}

impl Sandbox {
    /// Creates the Sandbox for the Session with id `session`, working in `workspace`. Nothing
    /// starts until the first command.
    #[must_use]
    pub fn new(containers: Arc<Containers>, session: &str, workspace: &Path) -> Self {
        Self {
            containers,
            session: session.to_owned(),
            workspace: workspace.to_owned(),
            starting: Mutex::new(()),
            started: AtomicBool::new(false),
        }
    }

    /// Creates a Sandbox that can never run anything, for when there must be no containers,
    /// such as in tests.
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(
            Arc::new(Containers::new(Vec::new())),
            "none",
            Path::new("/"),
        )
    }

    /// Returns the name of this Sandbox's container.
    fn name(&self) -> String {
        format!("sstack-{}", self.session)
    }

    /// Runs `script` with bash in the Workspace and returns what it did, stopping it after
    /// `limit`.
    ///
    /// # Errors
    /// Returns [`Failure::Unavailable`] if the Sandbox cannot run commands, and
    /// [`Failure::TimedOut`] if the command ran past `limit`.
    pub async fn run(&self, script: &str, limit: Duration) -> Result<Output, Failure> {
        let ready = self.containers.ready().await?;
        self.start(ready).await?;
        let name = self.name();
        let runtime = ready.runtime.as_ref();
        let abandoned = Abandoned::new(self, runtime);
        let argv = bounded(script, limit);
        let exec = runtime.exec(&name, &argv);
        let finished = tokio::time::timeout(limit + GRACE, exec).await;
        let output = finished.map_err(|_| Failure::TimedOut(limit))?;
        abandoned.disarm();
        let output = output.map_err(|error| {
            Failure::Unavailable(format!("{} did not run it: {error}", runtime.name()))
        })?;
        match output.status.code() {
            Some(TIMED_OUT_STATUS) => Err(Failure::TimedOut(limit)),
            _ => Ok(output),
        }
    }

    /// Makes sure this Sandbox's container runs the current image and writes files as the
    /// person, starting it if needed.
    ///
    /// A container that would write files as anyone else, such as root under a rootful
    /// runtime, is removed and the Sandbox is unavailable, so commands ask instead.
    async fn start(&self, ready: &Ready) -> Result<(), Failure> {
        let _only_one = self.starting.lock().await;
        if self.started.load(Ordering::SeqCst) {
            return Ok(());
        }
        let current = ready.runtime.inspect(&self.name()).await;
        if !current
            .as_ref()
            .is_some_and(|c| c.running && c.image == ready.image)
        {
            self.replace(ready, current.is_some()).await?;
        }
        if let Err(why) = self.check_owner(ready.runtime.as_ref()).await {
            ready.runtime.remove(&self.name()).await;
            return Err(Failure::Unavailable(why));
        }
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Writes a file in the Workspace from inside the container and checks on the host that it
    /// belongs to the person, deleting it again either way.
    ///
    /// This checks what the container really does, whatever the runtime or its settings.
    async fn check_owner(&self, runtime: &dyn ContainerRuntime) -> Result<(), String> {
        let probe = self
            .workspace
            .join(format!(".sstack-owner-check-{}", self.session));
        let touch = ["touch".to_owned(), probe.display().to_string()];
        let _ = runtime.exec(&self.name(), &touch).await;
        let owner = std::fs::symlink_metadata(&probe).map(|m| m.uid()).ok();
        let _ = std::fs::remove_file(&probe);
        owner_verdict(owner, getuid().as_raw())
    }

    /// Starts a fresh container, first removing the old one if `exists`. A container that
    /// fails to start is removed too, so that it is not left behind half made.
    async fn replace(&self, ready: &Ready, exists: bool) -> Result<(), Failure> {
        let name = self.name();
        if exists {
            ready.runtime.remove(&name).await;
        }
        let Err(error) = ready.runtime.start(&self.spec(&ready.image)).await else {
            return Ok(());
        };
        ready.runtime.remove(&name).await;
        Err(Failure::Unavailable(format!(
            "the Sandbox did not start: {error}"
        )))
    }

    /// Describes the container to start from the image tagged `image`.
    fn spec(&self, image: &str) -> ContainerSpec {
        let labels = [
            (SESSION_LABEL, self.session.clone()),
            (HARNESS_LABEL, std::process::id().to_string()),
            (IMAGE_LABEL, image.to_owned()),
        ];
        ContainerSpec {
            name: self.name(),
            image: image.to_owned(),
            workspace: self.workspace.clone(),
            masks: masks(&self.workspace),
            read_only: home::cargo_home().map_or_else(|_| Vec::new(), |h| crate_downloads(&h)),
            cache_volume: Some(CacheVolume {
                name: cache_volume(&self.workspace),
                mount: "/sccache".into(),
            }),
            labels: labels.map(|(k, v)| (k.to_owned(), v)).to_vec(),
            user: (getuid().as_raw(), getgid().as_raw()),
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let ready = self.containers.ready.get().and_then(|r| r.as_ref().ok());
        if let Some(ready) = ready.filter(|_| *self.started.get_mut()) {
            ready.runtime.remove_detached(&self.name());
        }
    }
}

/// Says whether a file the container wrote, owned by `owner` or missing, shows that it writes
/// as the person whose uid is `person`, and if not, why not.
fn owner_verdict(owner: Option<u32>, person: u32) -> Result<(), String> {
    match owner {
        Some(uid) if uid == person => Ok(()),
        Some(uid) => Err(format!(
            "files written in the Sandbox would belong to uid {uid}, not to you (uid {person})"
        )),
        None => Err("the Sandbox could not write to the Workspace".to_owned()),
    }
}

/// A guard that removes a Sandbox's container if the command running in it is abandoned before
/// it ends.
struct Abandoned<'a> {
    /// The Sandbox the command runs in.
    sandbox: &'a Sandbox,
    /// The runtime that runs it.
    runtime: &'a dyn ContainerRuntime,
    /// Whether the command is still running.
    running: bool,
}

impl<'a> Abandoned<'a> {
    /// Starts watching a command that is about to run in `sandbox`.
    fn new(sandbox: &'a Sandbox, runtime: &'a dyn ContainerRuntime) -> Self {
        Self {
            sandbox,
            runtime,
            running: true,
        }
    }

    /// Records that the command ended, so the container stays.
    fn disarm(mut self) {
        self.running = false;
    }
}

impl Drop for Abandoned<'_> {
    fn drop(&mut self) {
        if self.running {
            self.sandbox.started.store(false, Ordering::SeqCst);
            self.runtime.remove_detached(&self.sandbox.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::runtime::MockContainerRuntime;
    use super::*;
    use mockall::predicate::eq;
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    const LIMIT: Duration = Duration::from_secs(60);

    #[test]
    fn a_worktree_and_its_main_checkout_share_one_build_cache() {
        let main = Path::new("/projects/sstack");
        let worktree = main.join(".sstack/worktrees/7");
        assert_eq!(cache_volume(main), cache_volume(&worktree));
        assert_ne!(
            cache_volume(main),
            cache_volume(Path::new("/projects/other"))
        );
    }

    fn finished(code: i32, stdout: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }

    /// A runtime that works, already has the image, and writes files as the person.
    fn working() -> MockContainerRuntime {
        let mut runtime = MockContainerRuntime::new();
        runtime.expect_name().return_const("mock");
        runtime.expect_available().return_const(true);
        runtime.expect_sandboxes().returning(Vec::new);
        runtime.expect_has_image().return_const(true);
        runtime
            .expect_exec()
            .withf(|_, argv| argv[0] == "touch")
            .returning(|_, argv| {
                fs::write(&argv[1], "").unwrap();
                Ok(finished(0, ""))
            });
        runtime
    }

    /// Whether `argv` is a command the model asked for, not the Sandbox's own check.
    fn asked(argv: &[String]) -> bool {
        argv[0] == "timeout"
    }

    fn sandbox(runtime: MockContainerRuntime, workspace: &Path) -> Sandbox {
        let containers = Arc::new(Containers::new(vec![Arc::new(runtime)]));
        Sandbox::new(containers, "7", workspace)
    }

    fn running(image: String) -> Inspected {
        Inspected {
            running: true,
            image,
            harness: None,
        }
    }

    #[tokio::test]
    async fn the_first_runtime_that_works_is_used() {
        let mut missing = MockContainerRuntime::new();
        missing.expect_available().return_const(false);
        let containers = Containers::new(vec![Arc::new(missing), Arc::new(working())]);
        let ready = containers.ready().await.ok().unwrap();
        assert_eq!(ready.runtime.name(), "mock");
        assert_eq!(ready.image, image_tag());
    }

    #[tokio::test]
    async fn with_no_runtime_the_sandbox_says_which_it_tried() {
        let mut missing = MockContainerRuntime::new();
        missing.expect_available().return_const(false);
        missing.expect_name().return_const("podman");
        let containers = Arc::new(Containers::new(vec![Arc::new(missing)]));
        let sandbox = Sandbox::new(containers, "7", Path::new("/w"));
        let failure = sandbox.run("ls", LIMIT).await.unwrap_err();
        let expected = "no container runtime is available (tried podman)";
        assert_eq!(failure, Failure::Unavailable(expected.to_owned()));
    }

    #[tokio::test]
    async fn a_disabled_sandbox_runs_nothing() {
        let failure = Sandbox::disabled().run("ls", LIMIT).await.unwrap_err();
        let expected = Failure::Unavailable("the Sandbox is turned off".into());
        assert_eq!(failure, expected);
    }

    #[tokio::test]
    async fn containers_of_harnesses_that_have_gone_are_removed() {
        let mut runtime = MockContainerRuntime::new();
        let names = || {
            vec![
                "sstack-gone".into(),
                "sstack-live".into(),
                "sstack-x".into(),
            ]
        };
        runtime.expect_sandboxes().returning(names);
        let harness = |pid| {
            Some(Inspected {
                running: true,
                image: String::new(),
                harness: pid,
            })
        };
        let own = i32::try_from(std::process::id()).unwrap();
        runtime.expect_inspect().returning(move |name| match name {
            "sstack-gone" => harness(Some(i32::MAX)),
            "sstack-live" => harness(Some(own)),
            _ => harness(None),
        });
        runtime
            .expect_remove()
            .with(eq("sstack-gone"))
            .times(1)
            .return_const(());
        remove_orphans(&runtime).await;
    }

    #[tokio::test]
    async fn a_missing_image_is_built_from_the_containerfile() {
        let mut runtime = MockContainerRuntime::new();
        runtime.expect_has_image().return_const(false);
        runtime
            .expect_build_image()
            .withf(|tag, context| {
                let file = fs::read_to_string(context.join("Containerfile")).unwrap();
                tag == image_tag() && file == CONTAINERFILE
            })
            .times(1)
            .returning(|_, _| Ok(()));
        assert_eq!(ensure_image(&runtime, &image_tag()).await, Ok(()));
    }

    #[tokio::test]
    async fn an_image_that_does_not_build_makes_the_sandbox_unavailable() {
        let mut runtime = MockContainerRuntime::new();
        runtime.expect_has_image().return_const(false);
        runtime
            .expect_build_image()
            .returning(|_, _| Err(RuntimeError("no network".into())));
        let failure = ensure_image(&runtime, "sstack-sandbox:test").await;
        let expected = "the image did not build: no network";
        assert_eq!(failure, Err(Failure::Unavailable(expected.to_owned())));
    }

    #[tokio::test]
    async fn the_container_starts_once_and_commands_run_bounded_in_the_workspace() {
        let mut runtime = working();
        runtime.expect_inspect().times(1).return_const(None);
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_owned();
        runtime
            .expect_start()
            .withf(move |spec| spec.name == "sstack-7" && spec.workspace == workspace)
            .times(1)
            .returning(|_| Ok(()));
        runtime
            .expect_exec()
            .withf(|name, argv| {
                let bounded = ["timeout", "--kill-after=5s", "60s", "bash", "-c", "ls"];
                name == "sstack-7" && argv == bounded
            })
            .times(2)
            .returning(|_, _| Ok(finished(0, "a\n")));
        runtime.expect_remove_detached().times(1).return_const(());
        let sandbox = sandbox(runtime, dir.path());

        assert_eq!(sandbox.run("ls", LIMIT).await.unwrap().stdout, b"a\n");
        assert!(sandbox.run("ls", LIMIT).await.is_ok());
        drop(sandbox);
    }

    #[tokio::test]
    async fn a_running_container_with_the_current_image_is_used_again() {
        let mut runtime = working();
        runtime
            .expect_inspect()
            .return_const(Some(running(image_tag())));
        runtime.expect_start().never();
        runtime.expect_remove().never();
        runtime
            .expect_exec()
            .withf(|_, argv| asked(argv))
            .returning(|_, _| Ok(finished(0, "")));
        runtime.expect_remove_detached().return_const(());
        let dir = tempfile::tempdir().unwrap();
        assert!(sandbox(runtime, dir.path()).run("ls", LIMIT).await.is_ok());
    }

    #[tokio::test]
    async fn a_container_with_an_old_image_is_replaced() {
        let mut runtime = working();
        runtime
            .expect_inspect()
            .return_const(Some(running("old".into())));
        runtime.expect_remove().times(1).return_const(());
        runtime
            .expect_start()
            .withf(|spec| spec.image == image_tag())
            .times(1)
            .returning(|_| Ok(()));
        runtime
            .expect_exec()
            .withf(|_, argv| asked(argv))
            .returning(|_, _| Ok(finished(0, "")));
        runtime.expect_remove_detached().return_const(());
        let dir = tempfile::tempdir().unwrap();
        assert!(sandbox(runtime, dir.path()).run("ls", LIMIT).await.is_ok());
    }

    #[tokio::test]
    async fn a_container_that_does_not_start_makes_the_sandbox_unavailable() {
        let mut runtime = working();
        runtime.expect_inspect().return_const(None);
        runtime
            .expect_start()
            .returning(|_| Err(RuntimeError("no such image".into())));
        runtime.expect_remove().times(1).return_const(());
        let dir = tempfile::tempdir().unwrap();
        let failure = sandbox(runtime, dir.path()).run("ls", LIMIT).await;
        let expected = "the Sandbox did not start: no such image";
        assert_eq!(failure.unwrap_err(), Failure::Unavailable(expected.into()));
    }

    #[tokio::test]
    async fn a_command_stopped_by_its_limit_timed_out_and_one_the_runtime_lost_is_unavailable() {
        let mut runtime = working();
        runtime
            .expect_inspect()
            .return_const(Some(running(image_tag())));
        let mut results = vec![
            Err(std::io::Error::other("gone")),
            Ok(finished(TIMED_OUT_STATUS, "")),
        ];
        runtime
            .expect_exec()
            .withf(|_, argv| asked(argv))
            .returning(move |_, _| results.pop().unwrap());
        runtime.expect_remove_detached().return_const(());
        let dir = tempfile::tempdir().unwrap();
        let sandbox = sandbox(runtime, dir.path());

        let stopped = sandbox.run("sleep 99", LIMIT).await.unwrap_err();
        assert_eq!(stopped, Failure::TimedOut(LIMIT));
        let lost = sandbox.run("ls", LIMIT).await.unwrap_err();
        assert_eq!(
            lost,
            Failure::Unavailable("mock did not run it: gone".into())
        );
    }

    #[tokio::test]
    async fn a_container_that_cannot_write_as_the_person_is_removed_and_unavailable() {
        let mut runtime = MockContainerRuntime::new();
        runtime.expect_name().return_const("mock");
        runtime.expect_available().return_const(true);
        runtime.expect_sandboxes().returning(Vec::new);
        runtime.expect_has_image().return_const(true);
        runtime
            .expect_inspect()
            .return_const(Some(running(image_tag())));
        runtime
            .expect_exec()
            .withf(|_, argv| argv[0] == "touch")
            .returning(|_, _| Ok(finished(1, "")));
        runtime.expect_exec().withf(|_, argv| asked(argv)).never();
        runtime
            .expect_remove()
            .with(eq("sstack-7"))
            .times(1)
            .return_const(());
        runtime.expect_remove_detached().never();
        let dir = tempfile::tempdir().unwrap();
        let sandbox = sandbox(runtime, dir.path());

        let failure = sandbox.run("ls", LIMIT).await.unwrap_err();
        let expected = "the Sandbox could not write to the Workspace";
        assert_eq!(failure, Failure::Unavailable(expected.into()));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn only_files_that_belong_to_the_person_pass_the_owner_check() {
        assert_eq!(owner_verdict(Some(501), 501), Ok(()));
        let root = "files written in the Sandbox would belong to uid 0, not to you (uid 501)";
        assert_eq!(owner_verdict(Some(0), 501), Err(root.to_owned()));
        let missing = "the Sandbox could not write to the Workspace";
        assert_eq!(owner_verdict(None, 501), Err(missing.to_owned()));
    }

    #[tokio::test]
    async fn an_abandoned_command_takes_the_container_with_it() {
        let mut runtime = working();
        runtime
            .expect_remove_detached()
            .with(eq("sstack-7"))
            .times(1)
            .return_const(());
        let sandbox = sandbox(MockContainerRuntime::new(), Path::new("/w"));
        sandbox.started.store(true, Ordering::SeqCst);

        drop(Abandoned::new(&sandbox, &runtime));
        assert!(!sandbox.started.load(Ordering::SeqCst));
        Abandoned::new(&sandbox, &runtime).disarm();
    }

    #[test]
    fn protected_paths_are_masked_including_env_files_git_ignores() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".git"), "gitdir: elsewhere").unwrap();
        fs::create_dir_all(root.join(".sstack/worktrees/8")).unwrap();
        fs::write(root.join(".sstack/worktrees/8/.env"), "").unwrap();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join(".gitignore"), ".env*\n").unwrap();
        fs::write(root.join(".env"), "").unwrap();
        fs::write(root.join("app/.env.local"), "").unwrap();
        fs::write(root.join("app/main.rs"), "").unwrap();

        let mut found = masks(root);
        found.sort_by_key(|m| format!("{m:?}"));
        assert_eq!(
            found,
            [
                Mask::Directory(root.join(".sstack")),
                Mask::File(root.join(".env")),
                Mask::File(root.join(".git")),
                Mask::File(root.join("app/.env.local")),
            ]
        );
    }

    #[test]
    fn crate_downloads_that_exist_are_shared_where_the_image_expects_them() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("registry")).unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        let host = dir.path().canonicalize().unwrap().join("registry");
        let container = PathBuf::from("/cargo/registry");
        assert_eq!(crate_downloads(dir.path()), [ReadOnly { host, container }]);
        assert!(CONTAINERFILE.contains(&format!("CARGO_HOME={CONTAINER_CARGO_HOME} ")));
    }

    #[test]
    fn a_workspace_with_nothing_protected_has_no_masks() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(masks(dir.path()), []);
    }

    #[test]
    fn the_image_tag_follows_the_containerfile() {
        let tag = image_tag();
        assert!(tag.starts_with("sstack-sandbox:"));
        assert_eq!(tag.len(), "sstack-sandbox:".len() + 12);
    }
}
