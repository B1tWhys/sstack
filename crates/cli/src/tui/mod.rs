//! The interactive harness: a full-screen terminal UI built from components that react to events.
//!
//! Everything that happens arrives as an [`event::Event`] on one channel. Terminal input, the
//! Turns of every Session, and their questions about shell commands all arrive that way. The
//! [`app::App`] keeps a [`pane::Pane`] per Session and hands each event to the right one, whose
//! [`component::Component`]s keep their own state and ask for anything beyond themselves by
//! returning an [`event::Action`]. To add a part of the screen, write a component and give it a
//! place in the Pane or the App. To add a source of events, give it a clone of the
//! [`event::EventSender`].

mod app;
mod approver;
mod component;
mod components;
mod editor;
mod event;
mod input;
mod lifecycle;
mod pane;
#[cfg(test)]
mod test_support;
mod theme;
mod worker;

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use app::{App, Exit, Sessions};
use approver::EventApprover;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use event::{EventReceiver, EventSender, SessionSender};
use pane::Opened;
use sstack_core::skills::{self, Skill};
use sstack_core::tools::Approver;
use sstack_core::workspace::SessionSummary;
use sstack_core::{workspace, worktree};
use theme::ColorDepth;
use tokio::sync::mpsc;
use worker::AgentHandle;

use crate::describe;
use crate::rebuild::rebuild;
use crate::setup::{Place, Start, main_checkout, prepare, relaunch_args, sessions_directory};
use crate::{AnyError, Args};

/// Runs the interactive harness until the person quits, rebuilding when asked.
///
/// # Errors
/// Fails if the harness is not attached to a terminal, if the Session cannot be readied, or if
/// the terminal stops working.
pub async fn run(args: &Args) -> Result<ExitCode, AnyError> {
    require_terminal(io::stdin().is_terminal() && io::stdout().is_terminal())?;
    let (sender, mut events) = mpsc::unbounded_channel();
    let mut sessions = LiveSessions {
        args,
        events: sender.clone(),
        main: main_checkout()?,
        directory: sessions_directory()?,
    };
    let stored = sessions.stored()?;
    let first = sessions.open(starting_session(args, &stored))?;
    let mut app = App::builder()
        .first(first)
        .stored(stored)
        .sessions(Box::new(sessions))
        .colors(ColorDepth::detect())
        .build();
    app.notice(describe::skills_loaded(&load_skills()?));
    input::spawn(sender);
    while on_screen(&mut app, &mut events).await? == Exit::Rebuild {
        let failure = rebuild(&relaunch_args(args, app.session()));
        app.notice(format!("rebuild failed: {failure}"));
    }
    Ok(ExitCode::SUCCESS)
}

/// Picks what to show first: what the arguments ask for, or else a blank Session. One left unused
/// by an earlier run is reused, so that merely launching does not pile up worktrees.
fn starting_session<'a>(args: &'a Args, stored: &'a [SessionSummary]) -> Start<'a> {
    let unused = stored
        .iter()
        .find(|summary| summary.first_prompt.is_none())
        .and_then(|summary| summary.path.file_stem()?.to_str());
    let wanted = args.resume.as_deref().or(unused);
    wanted.map_or(Start::New(Place::Worktree), Start::Resume)
}

/// Opens and archives Sessions for real, on behalf of the App.
struct LiveSessions<'a> {
    /// The arguments of this run.
    args: &'a Args,
    /// Where each Session reports.
    events: EventSender,
    /// The main checkout of the project.
    main: PathBuf,
    /// Where the project's Sessions are stored.
    directory: PathBuf,
}

impl LiveSessions<'_> {
    /// Describes the Sessions worth listing: those with something in them, and blank ones with a
    /// worktree, which would otherwise be left behind unnoticed.
    fn stored(&self) -> Result<Vec<SessionSummary>, AnyError> {
        let mut stored = workspace::list_sessions(&self.directory)?;
        stored.retain(|summary| {
            let id = workspace::session_id(&summary.path);
            summary.first_prompt.is_some() || worktree::path(&self.main, &id).exists()
        });
        Ok(stored)
    }
}

impl Sessions for LiveSessions<'_> {
    fn open(&mut self, start: Start<'_>) -> Result<Opened, AnyError> {
        let events = &self.events;
        let report_to = |id: &str| SessionSender::new(id, events.clone());
        let approver =
            |id: &str| -> Arc<dyn Approver> { Arc::new(EventApprover::new(report_to(id))) };
        let run = prepare(self.args, start, approver)?;
        let id = workspace::session_id(&run.session);
        Ok(Opened {
            agent: AgentHandle::spawn(run.agent, report_to(&id)),
            path: run.session,
            notice: run.notice,
        })
    }

    fn check_archive(&self, id: &str) -> Result<(), AnyError> {
        match worktree::unfinished_work(&self.main, id)? {
            Some(reason) => Err(format!("{reason}: land it, or use /archive --force").into()),
            None => Ok(()),
        }
    }

    fn archive(&mut self, id: &str) -> Result<(), AnyError> {
        worktree::remove(&self.main, id)?;
        workspace::archive_session(&self.directory, id)?;
        Ok(())
    }
}

/// Finds the Skills installed for Claude Code or Codex, for this project and for the person.
///
/// # Errors
/// Fails only if the current directory cannot be found. Finding no Skills is not an error.
fn load_skills() -> Result<Vec<Skill>, AnyError> {
    let project = std::env::current_dir()?;
    let home = dirs::home_dir();
    let places = skills::locations(&project, home.as_deref(), |name| std::env::var_os(name));
    Ok(skills::discover(&places))
}

/// Refuses to start without a terminal, rather than take over one the harness was not given.
fn require_terminal(attached: bool) -> Result<(), AnyError> {
    if attached {
        Ok(())
    } else {
        Err("interactive mode needs a terminal; use --prompt to run without one".into())
    }
}

/// Runs the screen full screen, handing the terminal back however it stops.
async fn on_screen(app: &mut App<'_>, events: &mut EventReceiver) -> Result<Exit, AnyError> {
    let mut terminal = ratatui::try_init()?;
    let modes = InputModes::enable();
    let exit = app.run(&mut terminal, events).await;
    drop(modes);
    ratatui::restore();
    Ok(exit?)
}

/// Has the terminal report pastes in one piece and the mouse wheel, for as long as it lives.
///
/// Dropping it hands the terminal back as it was, including when the harness panics, so the
/// person's shell is never left receiving mouse reports. While it lives, most terminals select
/// text only with a modifier held, such as Shift, or Option on macOS.
struct InputModes;

impl InputModes {
    /// Turns the modes on. A terminal that refuses them still works, without those reports.
    fn enable() -> Self {
        let _ = execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture);
        Self
    }
}

impl Drop for InputModes {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launching_reuses_an_unused_session_unless_told_otherwise() {
        let summary = |id: &str, first_prompt: Option<&str>| SessionSummary {
            path: PathBuf::from(format!("/s/{id}.jsonl")),
            first_prompt: first_prompt.map(str::to_owned),
            modified: std::time::UNIX_EPOCH,
        };
        let stored = [summary("1", Some("hello")), summary("2", None)];
        let spoken = &stored[..1];

        let plain = crate::tests::args(&[]);
        assert_eq!(starting_session(&plain, &stored), Start::Resume("2"));
        let fresh = Start::New(Place::Worktree);
        assert_eq!(starting_session(&plain, spoken), fresh);
        let chosen = crate::tests::args(&["--resume", "1"]);
        assert_eq!(starting_session(&chosen, &stored), Start::Resume("1"));
    }

    #[test]
    fn only_a_terminal_can_run_interactively() {
        assert!(require_terminal(true).is_ok());
        let refused = require_terminal(false).unwrap_err().to_string();
        assert!(refused.contains("--prompt"));
    }
}
