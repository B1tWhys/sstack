//! End-to-end tests of the real binary against a local stand-in for the Claude API.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::json;
use sstack::session_metadata::SessionMetadata;

const SECRET_NAME: &str = "SSTACK_E2E_KEY";
const SECRET: &str = "e2e-secret-value";

fn event(name: &str, data: &str) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn stream(blocks: &str, stop_reason: &str) -> String {
    let start =
        r#"{"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":1}}}"#;
    let delta = format!(
        r#"{{"type":"message_delta","delta":{{"stop_reason":"{stop_reason}"}},"usage":{{"output_tokens":2}}}}"#
    );
    format!(
        "{}{blocks}{}{}",
        event("message_start", start),
        event("message_delta", &delta),
        event("message_stop", r#"{"type":"message_stop"}"#)
    )
}

fn text_reply(text: &str) -> String {
    let start =
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
    let delta = format!(
        r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{text}"}}}}"#
    );
    let blocks = format!(
        "{}{}{}",
        event("content_block_start", start),
        event("content_block_delta", &delta),
        event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#
        )
    );
    stream(&blocks, "end_turn")
}

fn thinking_then_text(thinking: &str, text: &str) -> String {
    let blocks = [
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": thinking}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "content_block_stop", "index": 1}),
    ];
    let blocks: String = blocks
        .iter()
        .map(|block| event(block["type"].as_str().unwrap(), &block.to_string()))
        .collect();
    stream(&blocks, "end_turn")
}

fn bash_call(command: &str) -> String {
    tool_call("bash", &serde_json::json!({"command": command}))
}

/// Returns a reply in which the model calls the tool `name` with `input`.
fn tool_call(name: &str, input: &serde_json::Value) -> String {
    let start = serde_json::json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "tool_use", "id": "c1", "name": name, "input": {}},
    });
    let delta = serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "input_json_delta", "partial_json": input.to_string()},
    });
    let blocks = format!(
        "{}{}{}",
        event("content_block_start", &start.to_string()),
        event("content_block_delta", &delta.to_string()),
        event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#
        )
    );
    stream(&blocks, "tool_use")
}

/// The model that names Sessions when settings choose no other.
const NAME_MODEL: &str = "claude-haiku-5-5";
/// The Session Name the stand-in for the Claude API gives every Session.
const SESSION_NAME: &str = "Named by the model";

/// A stand-in for the Claude API, so tests can script replies and inspect requests. Requests to
/// name a Session are answered with [`SESSION_NAME`] whenever they come, apart from the script.
struct FakeApi {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    namings: Arc<Mutex<Vec<String>>>,
}

impl FakeApi {
    fn start(replies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let namings = Arc::new(Mutex::new(Vec::new()));
        let (recorded, named) = (requests.clone(), namings.clone());
        std::thread::spawn(move || {
            let mut replies = replies.into_iter();
            for socket in listener.incoming() {
                let mut socket = socket.unwrap();
                let body = read_body(&mut socket);
                if body.contains(NAME_MODEL) {
                    named.lock().unwrap().push(body);
                    answer(&mut socket, &text_reply(SESSION_NAME));
                } else if let Some(reply) = replies.next() {
                    recorded.lock().unwrap().push(body);
                    answer(&mut socket, &reply);
                }
            }
        });
        Self {
            url,
            requests,
            namings,
        }
    }

    fn request(&self, index: usize) -> String {
        self.requests.lock().unwrap()[index].clone()
    }

    fn namings(&self) -> Vec<String> {
        self.namings.lock().unwrap().clone()
    }
}

fn answer(socket: &mut TcpStream, reply: &str) {
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reply.len()
    );
    socket.write_all(head.as_bytes()).unwrap();
    socket.write_all(reply.as_bytes()).unwrap();
}

fn read_body(socket: &mut TcpStream) -> String {
    let mut received = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let text = String::from_utf8_lossy(&received).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n")
            && body.len() >= content_length(head)
        {
            return body.to_owned();
        }
        let count = socket.read(&mut buffer).unwrap();
        received.extend_from_slice(&buffer[..count]);
    }
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length: ")?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// An isolated home directory and project, so tests never touch real settings or Sessions.
struct Sandbox {
    _home: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl Sandbox {
    fn new(url: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().canonicalize().unwrap();
        let project = path.join("project");
        fs::create_dir(&project).unwrap();
        let sandbox = Self {
            _home: home,
            home: path,
            project,
        };
        sandbox.write_settings(url);
        sandbox
    }

    fn write_settings(&self, url: &str) {
        let settings = format!(
            "default_connection = \"test\"\ndefault_model = \"claude-opus-5-5\"\n\n\
             [connections.test]\nprovider = \"anthropic\"\nsecret = \"{SECRET_NAME}\"\n\
             base_url = \"{url}\"\n"
        );
        let directory = config_dir(&self.home).join("sstack");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("config.toml"), settings).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sstack"));
        command
            .args(args)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env(SECRET_NAME, SECRET);
        command
    }

    /// Returns the Session Name of every named Session in the project.
    fn session_names(&self) -> Vec<String> {
        let sessions = data_dir(&self.home).join("sstack/sessions");
        let projects = fs::read_dir(sessions).unwrap();
        let files = projects.flat_map(|project| fs::read_dir(project.unwrap().path()).unwrap());
        files
            .map(|file| file.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .filter_map(|log| SessionMetadata::read(&log).unwrap().name)
            .collect()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn run_with_input(&self, input: &str) -> Output {
        let mut child = self
            .command(&[])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

fn config_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        home.join(".config")
    }
}

fn data_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        home.join(".local/share")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_headless_prompt_streams_a_reply_and_can_be_resumed() {
    let api = FakeApi::start(vec![text_reply("first reply"), text_reply("second reply")]);
    let sandbox = Sandbox::new(&api.url);

    let first = sandbox.run(&["--prompt", "remember this"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stdout(&first).contains("first reply"));

    let second = sandbox.run(&["--prompt", "what did I say?", "--resume"]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(stdout(&second).contains("second reply"));
    assert!(api.request(1).contains("remember this"));
    assert!(api.request(1).contains("first reply"));
}

#[test]
fn a_headless_run_names_its_new_session_after_the_prompt() {
    let api = FakeApi::start(vec![text_reply("first reply"), text_reply("second reply")]);
    let sandbox = Sandbox::new(&api.url);

    let first = sandbox.run(&["--prompt", "remember this"]);
    assert!(first.status.success(), "{}", stderr(&first));
    sandbox.run(&["--prompt", "what did I say?", "--resume"]);

    let namings = api.namings();
    assert_eq!(namings.len(), 1, "only the new Session is named");
    assert!(namings[0].contains("remember this"));
    assert_eq!(sandbox.session_names(), [SESSION_NAME]);
}

#[test]
fn piped_reasoning_has_no_escape_codes_and_ends_before_the_reply() {
    let api = FakeApi::start(vec![thinking_then_text("pondering", "the answer")]);
    let sandbox = Sandbox::new(&api.url);

    let output = sandbox.run(&["--prompt", "think"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let shown = stdout(&output);
    assert!(shown.contains("pondering\nthe answer"), "{shown}");
    assert!(!shown.contains('\x1b'), "{shown}");
}

#[test]
fn project_instructions_reach_the_model() {
    let api = FakeApi::start(vec![text_reply("ok")]);
    let sandbox = Sandbox::new(&api.url);
    fs::write(sandbox.project.join("AGENTS.md"), "Prefer tabs.").unwrap();

    sandbox.run(&["--prompt", "hi"]);

    assert!(api.request(0).contains("Prefer tabs."));
}

#[test]
fn user_instructions_reach_the_model() {
    let api = FakeApi::start(vec![text_reply("ok")]);
    let sandbox = Sandbox::new(&api.url);
    let directory = sandbox.home.join(".claude");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("CLAUDE.md"), "Never use emoji.").unwrap();

    sandbox.run(&["--prompt", "hi"]);

    assert!(api.request(0).contains("Never use emoji."));
}

#[test]
fn installed_skills_reach_the_model_with_their_files() {
    let skill = tempfile::tempdir().unwrap();
    let header = "---\nname: commits\ndescription: Use when committing.\n---\n";
    fs::write(
        skill.path().join("SKILL.md"),
        format!("{header}Keep titles short.\n"),
    )
    .unwrap();
    let notes = skill.path().join("notes.md");
    fs::write(&notes, "Say why.").unwrap();
    let call = tool_call("skill", &serde_json::json!({"name": "commits"}));
    let read = tool_call(
        "read",
        &serde_json::json!({"path": notes.display().to_string()}),
    );
    let api = FakeApi::start(vec![call, read, text_reply("ok")]);
    let sandbox = Sandbox::new(&api.url);
    let installed = sandbox.home.join(".agents/skills");
    fs::create_dir_all(installed.join("by-hand")).unwrap();
    std::os::unix::fs::symlink(skill.path(), installed.join("commits")).unwrap();
    let by_hand =
        "---\nname: by-hand\ndescription: Only by hand.\ndisable-model-invocation: true\n---\n";
    fs::write(installed.join("by-hand/SKILL.md"), by_hand).unwrap();

    sandbox.run(&["--prompt", "commit this"]);

    assert!(api.request(0).contains("- commits: Use when committing."));
    assert!(!api.request(0).contains("by-hand"));
    assert!(api.request(1).contains("Keep titles short."));
    assert!(!api.request(1).contains("name: commits"));
    assert!(api.request(2).contains("Say why."));
}

#[test]
fn a_headless_run_denies_shell_commands() {
    let api = FakeApi::start(vec![bash_call("touch ran"), text_reply("understood")]);
    let sandbox = Sandbox::new(&api.url);

    let output = sandbox.run(&["--prompt", "go"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!sandbox.project.join("ran").exists());
    assert!(api.request(1).contains("the user denied this command"));
}

#[test]
fn interactive_mode_without_a_terminal_fails_clearly() {
    let sandbox = Sandbox::new("http://127.0.0.1:1/v1/");

    let output = sandbox.run_with_input("hi\n/quit\n");

    assert!(!output.status.success());
    assert!(stderr(&output).contains("interactive mode needs a terminal"));
    assert!(stderr(&output).contains("--prompt"));
}

#[test]
fn a_dotenv_file_supplies_the_secret_but_stays_hidden_from_the_model() {
    let api = FakeApi::start(vec![text_reply("ok")]);
    let sandbox = Sandbox::new(&api.url);
    fs::write(
        sandbox.project.join(".env"),
        format!("{SECRET_NAME}=from-file\n"),
    )
    .unwrap();

    let output = sandbox
        .command(&["--prompt", "hi"])
        .env_remove(SECRET_NAME)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn a_missing_secret_and_an_unknown_connection_fail_clearly() {
    let sandbox = Sandbox::new("http://127.0.0.1:1/v1/");

    let no_secret = sandbox
        .command(&["--prompt", "hi"])
        .env_remove(SECRET_NAME)
        .output()
        .unwrap();
    assert!(!no_secret.status.success());
    assert!(stderr(&no_secret).contains(SECRET_NAME));

    let unknown = sandbox.run(&["--prompt", "hi", "--connection", "nope"]);
    assert!(!unknown.status.success());
    assert!(stderr(&unknown).contains("no connection named nope"));
}

#[test]
fn a_failed_request_is_reported_and_the_exit_code_shows_it() {
    let sandbox = Sandbox::new("http://127.0.0.1:1/v1/");
    let output = sandbox.run(&["--prompt", "hi"]);
    assert!(!output.status.success());
    assert!(stdout(&output).contains("error:"));
}
