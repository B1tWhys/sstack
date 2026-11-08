//! Helpers for testing the GUI with a scripted model and no real settings, Provider or Sandbox.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Result, bail};
use iced::futures::future::{self, BoxFuture, FutureExt};
use iced::widget::text_editor::{Action, Edit};
use sstack::agent::{Agent, AgentConfig};
use sstack::provider::scripted::{Scripted, ScriptedProvider};
use sstack::sandbox::Sandbox;
use sstack::session::{AssistantEntry, Entry, SessionLog, StopReason, ToolCall, Usage};
use sstack::session_report::ReportingApprover;
use sstack::session_setup::{Run, Start};
use sstack::tools::ToolBox;
use sstack::tools::project::Project;

use crate::reports::Reports;
use crate::window::{Message, Opened, Ready, Sessions};

/// Returns a model response that ends the Turn with `text`.
pub fn reply(text: &str) -> AssistantEntry {
    AssistantEntry {
        text: text.into(),
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
        payload: None,
    }
}

/// Returns a model response that asks to run `command` in the shell. The Sandbox is off in
/// tests, so the person is asked whether it may run on the host.
pub fn bash_call(command: &str) -> AssistantEntry {
    AssistantEntry {
        tool_calls: vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            input: serde_json::json!({ "command": command }),
        }],
        stop: StopReason::ToolUse,
        ..reply("")
    }
}

/// Returns where the Session with id `id` is stored in `dir`.
pub fn path_of(dir: &Path, id: &str) -> PathBuf {
    dir.join(id).with_extension("jsonl")
}

/// Stores a Session with id `id` in `dir`, already holding `entries`.
pub fn store(dir: &Path, id: &str, entries: &[Entry]) {
    let mut log = SessionLog::create(&path_of(dir, id)).unwrap();
    for entry in entries {
        log.append(entry).unwrap();
    }
}

/// Opens the Session stored at `path`, working in `dir`, whose model gives the `script`ed
/// responses in order. Shell commands are asked about through the Session's reports, as they
/// are outside tests.
fn open_stored(dir: &Path, path: PathBuf, script: Vec<Scripted>) -> Opened {
    let (log, entries) = SessionLog::open(&path).unwrap();
    let (reports, sender) = Reports::new();
    let config = AgentConfig {
        provider: Arc::new(ScriptedProvider::new(script)),
        model: "scripted".into(),
        system: String::new(),
        tools: ToolBox::standard()
            .project(Arc::new(Project::new(dir).unwrap()))
            .sandbox(Arc::new(Sandbox::disabled()))
            .approver(Arc::new(ReportingApprover::new(sender.clone())))
            .call(),
        context_limit: 1000,
    };
    let run = Run {
        agent: Agent::new(config, log, entries).unwrap(),
        session: path,
        notices: Vec::new(),
        descriptions_left_out: 0,
        vim: false,
        skills: Vec::new(),
    };
    Opened {
        run,
        reports,
        sender,
    }
}

/// Returns a window readied in `dir` on one Session, with id 1, whose model gives the
/// `script`ed responses in order. `entries` are already in the Session's log. Sessions the
/// window opens later come from a [`TestSessions`] and have no script.
pub fn readied(dir: &Path, script: Vec<Scripted>, entries: &[Entry]) -> Ready {
    store(dir, "1", entries);
    Ready {
        first: open_stored(dir, path_of(dir, "1"), script),
        stored: Vec::new(),
        sessions: Box::new(TestSessions::new(dir)),
        build: never_built,
        relaunch: Arc::default(),
    }
}

/// What a test's [`TestSessions`] did, which the test keeps a handle on after the window takes
/// them.
#[derive(Debug, Default)]
pub struct Done {
    /// The ids of the Sessions put away, in order.
    pub archived: Vec<String>,
}

/// Sessions stored in a test's directory, opened with scripted models.
pub struct TestSessions {
    /// Where the Sessions are stored, and where they work.
    dir: PathBuf,
    /// The scripts of the Sessions opened from now on, in order. Sessions opened once these run
    /// out get an empty script.
    pub scripts: VecDeque<Vec<Scripted>>,
    /// The id the next new Session gets.
    next: u32,
    /// Why archiving would lose work, if it would.
    pub unfinished: Option<String>,
    /// Whether opening any Session fails.
    pub broken: bool,
    /// What these Sessions did, shared with the test.
    pub done: Rc<RefCell<Done>>,
}

impl TestSessions {
    /// Creates the Sessions stored in `dir`, where the first new one gets id 2.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            scripts: VecDeque::new(),
            next: 2,
            unfinished: None,
            broken: false,
            done: Rc::default(),
        }
    }
}

impl Sessions for TestSessions {
    fn open(&mut self, start: Start<'_>) -> Result<Opened> {
        if self.broken {
            bail!("the Session is open elsewhere");
        }
        let id = match start {
            Start::Resume(id) => id.to_owned(),
            Start::New(_) => {
                let id = self.next.to_string();
                self.next += 1;
                store(&self.dir, &id, &[]);
                id
            }
        };
        let script = self.scripts.pop_front().unwrap_or_default();
        Ok(open_stored(&self.dir, path_of(&self.dir, &id), script))
    }

    fn check_archive(&self, _id: &str) -> Result<()> {
        match &self.unfinished {
            Some(reason) => bail!("{reason}"),
            None => Ok(()),
        }
    }

    fn archive(&mut self, id: &str) -> Result<()> {
        self.done.borrow_mut().archived.push(id.to_owned());
        Ok(())
    }

    fn rename(&mut self, _id: &str, name: Option<&str>) -> Result<()> {
        if name == Some("unstorable") {
            bail!("the disk is full");
        }
        Ok(())
    }
}

/// A build that never ends and never runs cargo, so that a test decides how a Rebuild's build
/// ends by sending [`Message::Built`] itself.
fn never_built() -> BoxFuture<'static, Result<PathBuf, String>> {
    future::pending().boxed()
}

/// Returns the messages that replace whatever is in the prompt with `text`.
pub fn typed(text: &str) -> [Message; 2] {
    let paste = Edit::Paste(Arc::new(text.to_owned()));
    [
        Message::Edit(Action::SelectAll),
        Message::Edit(Action::Edit(paste)),
    ]
}
