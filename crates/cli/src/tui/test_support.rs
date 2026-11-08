//! Helpers for testing components and the [`App`](crate::tui::app::App) without a real terminal.

use std::path::Path;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::{Frame, Terminal};
use sstack::agent::{Agent, AgentConfig};
use sstack::provider::Provider;
use sstack::sandbox::Sandbox;
use sstack::session::{AssistantEntry, SessionLog, StopReason, ToolCall, Usage};
use sstack::skills::{self, Location, Scope};
use sstack::tools::project::Project;
use sstack::tools::{Approver, ToolBox};
use sstack::workspace;

use super::approver::EventApprover;
use super::component::{Component, KeyOutcome};
use super::event::{EventSender, SessionSender};
use super::pane::Opened;
use super::worker::AgentHandle;

/// Returns a press of `code` with no modifiers.
pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Returns a press of `character` with Control held.
pub fn ctrl(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

/// Returns a press of `character` with Alt held.
pub fn alt(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::ALT)
}

/// Offers each character of `text` to `component` as a key press, and checks that each was used.
pub fn type_into(component: &mut impl Component, text: &str) {
    for character in text.chars() {
        let outcome = component.key(key(KeyCode::Char(character)));
        assert_eq!(outcome, KeyOutcome::Handled, "{character}");
    }
}

/// Draws onto a blank screen of the given size, returning each row with trailing spaces removed.
pub fn screen(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> Vec<String> {
    rows(&drawn(width, height, draw))
}

/// Draws onto a blank test terminal of the given size and returns it, ready for a snapshot.
///
/// Its backend displays one quoted line per row, so a snapshot shows trailing spaces and blank
/// rows that a plain listing of the rows would lose.
pub fn drawn(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(draw).unwrap();
    terminal
}

/// Returns each row on a test terminal, with trailing spaces removed.
pub fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let width = usize::from(buffer.area.width);
    let row = |cells: &[ratatui::buffer::Cell]| {
        let text: String = cells.iter().map(ratatui::buffer::Cell::symbol).collect();
        text.trim_end().to_owned()
    };
    buffer.content.chunks(width).map(row).collect()
}

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

/// Returns a model response asking to run `command` in the shell.
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

/// Opens the Session stored at `path`, new or existing, with an agent working in its directory
/// and reporting to `events`.
pub fn opened(path: &Path, provider: Arc<dyn Provider>, events: &EventSender) -> Opened {
    let id = workspace::session_id(path);
    let approver = Arc::new(EventApprover::new(SessionSender::new(&id, events.clone())));
    let (log, entries) = if path.exists() {
        SessionLog::open(path).unwrap()
    } else {
        (SessionLog::create(path).unwrap(), vec![])
    };
    let config = agent_config(path.parent().unwrap(), provider, approver);
    let agent = Agent::new(config, log, entries).unwrap();
    Opened {
        agent: AgentHandle::spawn(agent, SessionSender::new(&id, events.clone())),
        path: path.to_owned(),
        notices: Vec::new(),
        events: SessionSender::new(&id, events.clone()),
        vim: false,
        skills: skills::discover(&[Location {
            directory: path.parent().unwrap().join(".agents/skills"),
            scope: Scope::Project,
        }]),
    }
}

/// Returns what a test agent working in `directory` runs on, with no system prompt.
fn agent_config(
    directory: &Path,
    provider: Arc<dyn Provider>,
    approver: Arc<dyn Approver>,
) -> AgentConfig {
    let project = Arc::new(Project::new(directory).unwrap());
    AgentConfig {
        provider,
        model: "scripted".into(),
        system: String::new(),
        tools: ToolBox::standard()
            .project(project)
            .sandbox(Arc::new(Sandbox::disabled()))
            .approver(approver)
            .call(),
        context_limit: 1000,
    }
}
