//! Helpers for testing the GUI with a scripted model and no real settings, Provider or Sandbox.

use std::path::Path;
use std::sync::Arc;

use iced::widget::text_editor::{Action, Edit};
use sstack::agent::{Agent, AgentConfig};
use sstack::provider::scripted::{Scripted, ScriptedProvider};
use sstack::sandbox::Sandbox;
use sstack::session::{AssistantEntry, Entry, SessionLog, StopReason, Usage};
use sstack::session_setup::Run;
use sstack::tools::project::Project;
use sstack::tools::{DenyAll, ToolBox};

use crate::window::Message;

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

/// Returns a Session readied in `dir`, with id 1, whose model gives the `script`ed responses in
/// order. `entries` are already in the Session's log.
pub fn readied(dir: &Path, script: Vec<Scripted>, entries: &[Entry]) -> Run {
    let path = dir.join("1").with_extension("jsonl");
    let mut log = SessionLog::create(&path).unwrap();
    for entry in entries {
        log.append(entry).unwrap();
    }
    drop(log);
    let (log, entries) = SessionLog::open(&path).unwrap();
    let config = AgentConfig {
        provider: Arc::new(ScriptedProvider::new(script)),
        model: "scripted".into(),
        system: String::new(),
        tools: ToolBox::standard()
            .project(Arc::new(Project::new(dir).unwrap()))
            .sandbox(Arc::new(Sandbox::disabled()))
            .approver(Arc::new(DenyAll))
            .call(),
        context_limit: 1000,
    };
    Run {
        agent: Agent::new(config, log, entries).unwrap(),
        session: path,
        notices: Vec::new(),
        descriptions_left_out: 0,
        vim: false,
        skills: Vec::new(),
    }
}

/// Returns the messages that replace whatever is in the prompt with `text`.
pub fn typed(text: &str) -> [Message; 2] {
    let paste = Edit::Paste(Arc::new(text.to_owned()));
    [
        Message::Edit(Action::SelectAll),
        Message::Edit(Action::Edit(paste)),
    ]
}
