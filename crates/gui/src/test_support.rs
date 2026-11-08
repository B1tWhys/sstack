//! Helpers for testing the GUI with a scripted model and no real settings, Provider or Sandbox.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::futures::future::{self, BoxFuture, FutureExt};
use iced::widget::text_editor::{Action, Edit};
use sstack::agent::{Agent, AgentConfig};
use sstack::provider::scripted::{Scripted, ScriptedProvider};
use sstack::sandbox::Sandbox;
use sstack::session::{AssistantEntry, Entry, SessionLog, StopReason, ToolCall, Usage};
use sstack::session_report::ReportingApprover;
use sstack::session_setup::Run;
use sstack::tools::ToolBox;
use sstack::tools::project::Project;

use crate::reports::Reports;
use crate::window::{Message, Ready};

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

/// Returns a Session readied in `dir`, with id 1, whose model gives the `script`ed responses in
/// order. `entries` are already in the Session's log. Shell commands are asked about through the
/// Session's reports, as they are outside tests.
pub fn readied(dir: &Path, script: Vec<Scripted>, entries: &[Entry]) -> Ready {
    let path = dir.join("1").with_extension("jsonl");
    let mut log = SessionLog::create(&path).unwrap();
    for entry in entries {
        log.append(entry).unwrap();
    }
    drop(log);
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
    Ready {
        run,
        reports,
        sender,
        build: never_built,
        relaunch: Arc::default(),
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
