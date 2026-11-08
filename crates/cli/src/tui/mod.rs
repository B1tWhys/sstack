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
mod component;
mod components;
mod editor;
mod event;
mod input;
mod pane;
#[cfg(test)]
mod test_support;
mod theme;

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Result, bail, ensure};
use app::{App, Sessions};
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use event::{Event, EventReceiver, EventSender, SessionSender};
use pane::Opened;
use sstack::rebuild;
use sstack::running_harness::Exit;
use sstack::sandbox::Containers;
use sstack::session_report::{ReportingApprover, SessionReport};
use sstack::session_setup::{
    Start, installed_skills, listed_sessions, main_checkout, prepare, sessions_directory,
    starting_session,
};
use sstack::tools::Approver;
use sstack::turn_runner::AgentHandle;
use sstack::{workspace, worktree};
use theme::ColorDepth;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::describe;
use crate::{Args, HARNESS};

/// Runs the interactive harness until the person quits, rebuilding when asked.
///
/// # Errors
/// Fails if the harness is not attached to a terminal, if the Session cannot be readied, or if
/// the terminal stops working.
pub async fn run(args: &Args) -> Result<ExitCode> {
    require_terminal(io::stdin().is_terminal() && io::stdout().is_terminal())?;
    let (sender, mut events) = mpsc::unbounded_channel();
    let started_in = std::env::current_dir()?;
    let main = main_checkout(&started_in)?;
    let mut sessions = LiveSessions {
        args,
        events: sender.clone(),
        directory: sessions_directory(&started_in)?,
        main: main.clone(),
        started_in: started_in.clone(),
        build: None,
        containers: Arc::new(Containers::detect()),
    };
    let stored = listed_sessions(&main, &sessions.directory)?;
    let first = sessions.open(starting_session(args.resume.as_deref(), &stored))?;
    let mut app = App::builder()
        .first(first)
        .stored(stored)
        .sessions(Box::new(sessions))
        .colors(ColorDepth::detect())
        .build();
    let found = installed_skills(&main, &started_in);
    app.notice(describe::skills_loaded(&found));
    app.continue_parked();
    input::spawn(sender);
    while let Exit::Rebuild(binary) = on_screen(&mut app, &mut events).await? {
        let failure = rebuild::relaunch(&binary, &args.relaunch(app.session()));
        app.relaunch_failed(&failure.to_string());
    }
    Ok(ExitCode::SUCCESS)
}

/// Real Sessions, opened and archived on behalf of the [`App`].
struct LiveSessions<'a> {
    /// The arguments of this run.
    args: &'a Args,
    /// Where each Session reports.
    events: EventSender,
    /// The main checkout of the project.
    main: PathBuf,
    /// The directory the person started the harness in, inside the project.
    started_in: PathBuf,
    /// Where the project's Sessions are stored.
    directory: PathBuf,
    /// The container runtime shared by every Session's Sandbox, so that the runtime is chosen and
    /// the image built once.
    containers: Arc<Containers>,
    /// The build in progress. Aborting it stops the build.
    build: Option<JoinHandle<()>>,
}

impl Sessions for LiveSessions<'_> {
    fn open(&mut self, start: Start<'_>) -> Result<Opened> {
        let events = &self.events;
        let report_to = |id: &str| SessionSender::new(id, events.clone());
        let approver = |id: &str| -> Arc<dyn Approver> {
            Arc::new(ReportingApprover::new(report_to(id).reporter()))
        };
        let run = prepare()
            .start(start)
            .directory(&self.started_in)
            .maybe_connection(self.args.connection.as_deref())
            .maybe_model(self.args.model.as_deref())
            .approver(approver)
            .containers(self.containers.clone())
            .call()?;
        let id = workspace::session_id(&run.session);
        Ok(Opened {
            agent: AgentHandle::spawn(run.agent, report_to(&id).reporter()),
            path: run.session,
            notices: run
                .notices
                .into_iter()
                .chain(describe::listing_shortened(run.descriptions_left_out))
                .collect(),
            events: report_to(&id),
            vim: run.vim,
            skills: run.skills,
        })
    }

    fn check_archive(&self, id: &str) -> Result<()> {
        match worktree::unfinished_work(&self.main, id)? {
            Some(reason) => bail!("{reason}: land it, or use /archive --force"),
            None => Ok(()),
        }
    }

    fn archive(&mut self, id: &str) -> Result<()> {
        worktree::remove(&self.main, id)?;
        workspace::archive_session(&self.directory, id)?;
        Ok(())
    }

    fn rename(&mut self, id: &str, name: Option<&str>) -> Result<()> {
        Ok(workspace::name_session(&self.directory, id, name)?)
    }

    fn start_build(&mut self) {
        let events = self.events.clone();
        self.build = Some(tokio::spawn(async move {
            let _ = events.send(Event::Report(SessionReport::BuildFinished(
                rebuild::build(HARNESS).await,
            )));
        }));
    }

    fn cancel_build(&mut self) {
        if let Some(build) = self.build.take() {
            build.abort();
        }
    }
}

/// Refuses to start without a terminal, rather than take over one the harness was not given.
fn require_terminal(attached: bool) -> Result<()> {
    ensure!(
        attached,
        "interactive mode needs a terminal; use --prompt to run without one"
    );
    Ok(())
}

/// Runs the screen full screen, handing the terminal back however it stops.
async fn on_screen(app: &mut App<'_>, events: &mut EventReceiver) -> Result<Exit> {
    let mut terminal = ratatui::try_init()?;
    let modes = InputModes::enable();
    let exit = app.run(&mut terminal, events).await;
    drop(modes);
    ratatui::restore();
    Ok(exit?)
}

/// The terminal modes that report pastes in one piece and the mouse wheel, which stay on for as
/// long as this value lives.
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
        let _ = execute!(
            io::stdout(),
            SetCursorStyle::DefaultUserShape,
            DisableMouseCapture,
            DisableBracketedPaste
        );
    }
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
