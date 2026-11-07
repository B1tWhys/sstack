//! Helpers for testing components and the App without a real terminal.

use std::path::Path;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::{Frame, Terminal};
use sstack_core::agent::{Agent, AgentConfig};
use sstack_core::provider::Provider;
use sstack_core::session::{AssistantEntry, SessionLog, StopReason, ToolCall, Usage};
use sstack_core::tools::project::Project;
use sstack_core::tools::{Approver, ToolBox};

use super::component::{Component, KeyOutcome};

/// Returns a press of `code` with no modifiers.
pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Returns a press of `character` with Control held.
pub fn ctrl(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
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
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(draw).unwrap();
    rows(&terminal)
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

/// Returns an agent working in `directory`, recording its Session at `session`.
pub fn agent(directory: &Path, provider: Arc<dyn Provider>, approver: Arc<dyn Approver>) -> Agent {
    let project = Arc::new(Project::new(directory).unwrap());
    let config = AgentConfig {
        provider,
        model: "scripted".into(),
        system: String::new(),
        tools: ToolBox::standard(project, approver, vec![]),
        context_limit: 1000,
    };
    let log = SessionLog::create(&directory.join("session.jsonl")).unwrap();
    Agent::new(config, log, vec![]).unwrap()
}
