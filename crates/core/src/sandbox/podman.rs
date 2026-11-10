//! Tests of the Sandbox on a real Podman, which must be installed and running.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::*;

const LIMIT: Duration = Duration::from_secs(60);

/// A Workspace for one test, under the build directory, since Podman on macOS can only mount
/// directories inside the person's home.
fn workspace() -> (tempfile::TempDir, PathBuf) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/sandbox-tests");
    fs::create_dir_all(&parent).unwrap();
    let dir = tempfile::tempdir_in(parent).unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

/// A Sandbox on Podman for a Session named after its Workspace.
fn sandbox(root: &Path) -> Sandbox {
    let podman: Arc<dyn ContainerRuntime> = Arc::new(DockerCli::new("podman"));
    let containers = Arc::new(Containers::new(vec![podman]));
    let session = root.file_name().unwrap().to_string_lossy().replace('.', "");
    Sandbox::new(containers, &session, root)
}

/// Runs `script` in `sandbox`, returning what it printed.
async fn run(sandbox: &Sandbox, script: &str) -> String {
    let output = sandbox.run(script, LIMIT).await.unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{script}: {stderr}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The ownership check only means something on Linux. On macOS the Podman VM shares the
/// Workspace with a file system that gives every file to the person, whichever uid wrote it,
/// so it passes there even if the container ran as some other user.
#[tokio::test]
async fn commands_change_the_workspace_as_the_person() {
    let (_dir, root) = workspace();
    let sandbox = sandbox(&root);

    let pwd = run(&sandbox, "pwd; echo hi > written").await;

    assert_eq!(pwd.trim(), root.display().to_string());
    let written = root.join("written");
    assert_eq!(fs::read_to_string(&written).unwrap(), "hi\n");
    let owner = fs::metadata(&written).unwrap().uid();
    assert_eq!(owner, fs::metadata(&root).unwrap().uid());
}

#[tokio::test]
async fn protected_paths_and_the_network_are_out_of_reach() {
    let (_dir, root) = workspace();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(root.join(".git/config"), "secret").unwrap();
    fs::write(root.join(".env"), "TOKEN=secret").unwrap();
    let sandbox = sandbox(&root);

    let seen = run(&sandbox, "ls -A .git; cat .env; echo end").await;
    let connect = "python3 -c 'import socket; socket.create_connection((\"1.1.1.1\", 80), 2)'";
    let offline = sandbox.run(connect, LIMIT).await.unwrap();
    let escape = sandbox.run("touch .git/hook", LIMIT).await.unwrap();

    assert_eq!(seen, "end\n");
    assert!(!offline.status.success());
    assert!(!escape.status.success());
    assert!(!root.join(".git/hook").exists());
}

#[tokio::test]
async fn the_image_has_the_tools_the_model_is_told_about() {
    let (_dir, root) = workspace();
    let sandbox = sandbox(&root);
    let tools = concat!(
        "rg fd jq python3 patch file tree xxd bc less git nvim cc cargo rustc ",
        "cargo-nextest cargo-llvm-cov cargo-insta cargo-crap4rust"
    );
    let missing = run(
        &sandbox,
        &format!("for t in {tools}; do command -v $t >/dev/null || echo $t; done"),
    )
    .await;
    assert_eq!(missing, "");
}

/// Assumes the host has downloaded crates before, as any machine running these tests has.
#[tokio::test]
async fn cargo_builds_offline_outside_the_workspace_and_cannot_change_the_downloads() {
    let (_dir, root) = workspace();
    let manifest = "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
    fs::write(root.join("Cargo.toml"), manifest).unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    let sandbox = sandbox(&root);

    run(
        &sandbox,
        "cargo build --quiet && test -x /target/debug/probe",
    )
    .await;
    run(&sandbox, "test -d /cargo/registry/cache").await;
    let planted = sandbox.run("touch /cargo/registry/planted", LIMIT).await;

    assert!(!root.join("target").exists());
    assert!(!planted.unwrap().status.success());
}

#[tokio::test]
async fn a_command_past_its_limit_is_stopped() {
    let (_dir, root) = workspace();
    let sandbox = sandbox(&root);
    let failure = sandbox.run("sleep 30", Duration::from_secs(1)).await;
    assert_eq!(
        failure.unwrap_err(),
        Failure::TimedOut(Duration::from_secs(1))
    );
}
