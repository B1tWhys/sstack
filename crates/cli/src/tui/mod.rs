//! The interactive harness: a full-screen terminal UI built from components that react to events.
//!
//! Everything that happens arrives as an [`event::Event`] on one channel. Terminal input, the
//! agent's Turns, and its questions about shell commands all arrive that way. The [`app::App`]
//! hands each event to its [`component::Component`]s, which keep their own state and ask for
//! anything beyond themselves by returning an [`event::Action`]. To add a part of the screen,
//! write a component and give it a place in the App. To add a source of events, give it a clone
//! of the [`event::EventSender`].

mod app;
mod approver;
mod component;
mod components;
mod editor;
mod event;
mod input;
#[cfg(test)]
mod test_support;
mod worker;

use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::sync::Arc;

use app::{App, Exit};
use approver::EventApprover;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use event::EventReceiver;
use sstack_core::workspace;
use tokio::sync::mpsc;
use worker::AgentHandle;

use crate::rebuild::rebuild;
use crate::setup::{Run, prepare, relaunch_args};
use crate::{AnyError, Args};

/// Runs the interactive harness until the person quits, rebuilding when asked.
///
/// # Errors
/// Fails if the harness is not attached to a terminal, if the Session cannot be readied, or if
/// the terminal stops working.
pub async fn run(args: &Args) -> Result<ExitCode, AnyError> {
    require_terminal(io::stdin().is_terminal() && io::stdout().is_terminal())?;
    let (sender, mut events) = mpsc::unbounded_channel();
    let Run { agent, session } = prepare(args, Arc::new(EventApprover::new(sender.clone())))?;
    let agent = AgentHandle::spawn(agent, sender.clone());
    let mut app = App::new(agent, workspace::session_id(&session));
    input::spawn(sender);
    while on_screen(&mut app, &mut events).await? == Exit::Rebuild {
        let failure = rebuild(&relaunch_args(args, &session));
        app.notice(format!("rebuild failed: {failure}"));
    }
    Ok(ExitCode::SUCCESS)
}

/// Refuses to start without a terminal, rather than take over one the harness was not given.
fn require_terminal(attached: bool) -> Result<(), AnyError> {
    if attached {
        Ok(())
    } else {
        Err("interactive mode needs a terminal; use --prompt to run without one".into())
    }
}

/// Runs the App full screen, handing the terminal back however it stops.
async fn on_screen(app: &mut App, events: &mut EventReceiver) -> Result<Exit, AnyError> {
    let mut terminal = ratatui::try_init()?;
    let _ = execute!(io::stdout(), EnableBracketedPaste);
    let exit = app.run(&mut terminal, events).await;
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    Ok(exit?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_terminal_can_run_interactively() {
        assert!(require_terminal(true).is_ok());
        let refused = require_terminal(false).unwrap_err().to_string();
        assert!(refused.contains("--prompt"));
    }
}
