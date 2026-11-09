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
mod archiver;
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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Result, bail, ensure};
use app::{App, Sessions};
use archiver::Archiver;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::style::Print;
use event::{Event, EventReceiver, EventSender, SessionSender};
use pane::Opened;
use sstack::rebuild::{self, Checkout};
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
        archiver: Archiver::default(),
    };
    let archiver = sessions.archiver.clone();
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
        archiver.finish().await;
        let failure = rebuild::relaunch(&binary, &args.relaunch(app.session()));
        app.relaunch_failed(&failure.to_string());
    }
    archiver.finish().await;
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
    /// Where Sessions are put away in the background.
    archiver: Archiver,
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
            naming: run.naming,
        })
    }

    fn check_archive(&self, id: &str) -> Result<()> {
        match worktree::unfinished_work(&self.main, id)? {
            Some(reason) => bail!("{reason}: land it, or use /archive --force"),
            None => Ok(()),
        }
    }

    fn archive(&mut self, id: &str) {
        let (main, directory) = (self.main.clone(), self.directory.clone());
        let (events, id) = (self.events.clone(), id.to_owned());
        self.archiver.start(move || {
            let outcome = put_away(&main, &directory, &id).map_err(|error| error.to_string());
            let _ = events.send(Event::Archived { id, outcome });
        });
    }

    fn rename(&mut self, id: &str, name: Option<&str>) -> Result<()> {
        Ok(workspace::name_session(&self.directory, id, name)?)
    }

    fn start_build(&mut self, checkout: Checkout, id: &str) {
        let events = self.events.clone();
        let source = rebuild::source(checkout, &worktree::path(&self.main, id));
        self.build = Some(tokio::spawn(async move {
            let built = match source {
                Ok(source) => rebuild::build(HARNESS, source).await,
                Err(failure) => Err(failure),
            };
            let _ = events.send(Event::Report(SessionReport::BuildFinished(built)));
        }));
    }

    fn cancel_build(&mut self) {
        if let Some(build) = self.build.take() {
            build.abort();
        }
    }
}

/// Deletes the worktree of the Session with id `id` from the project checked out at `main`, and
/// moves its log out of `directory` into the archive. Blocks until done, which can take seconds.
///
/// # Errors
/// Returns an error if git refuses to delete the worktree, or the log cannot be moved. If the
/// worktree survives, the log stays where it was.
fn put_away(main: &Path, directory: &Path, id: &str) -> Result<()> {
    worktree::remove(main, id)?;
    workspace::archive_session(directory, id)?;
    Ok(())
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
    let title = SavedTitle::save();
    let exit = app.run(&mut terminal, events).await;
    drop(title);
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

/// The title the terminal showed before the harness titled it after its Sessions, which comes back
/// once this value is dropped, including when the harness panics.
///
/// It uses the xterm title stack, which most terminals keep. In one that does not, the last
/// Session's title stays.
struct SavedTitle;

/// The escape sequence that pushes the terminal's title onto its title stack.
const PUSH_TITLE: &str = "\x1b[22;0t";
/// The escape sequence that pops the terminal's title back off its title stack.
const POP_TITLE: &str = "\x1b[23;0t";

impl SavedTitle {
    /// Saves the terminal's title. A terminal that cannot save it still works.
    fn save() -> Self {
        let _ = execute!(io::stdout(), Print(PUSH_TITLE));
        Self
    }
}

impl Drop for SavedTitle {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Print(POP_TITLE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn putting_away_moves_the_log_into_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let directory = dir.path().join("sessions");
        std::fs::create_dir_all(&directory).unwrap();
        let log = workspace::session_path(&directory, "1");
        std::fs::write(&log, "").unwrap();

        put_away(dir.path(), &directory, "1").unwrap();

        assert!(!log.exists());
        assert!(workspace::restore_session(&directory, "1").unwrap());
    }

    #[test]
    fn only_a_terminal_can_run_interactively() {
        assert!(require_terminal(true).is_ok());
        let refused = require_terminal(false).unwrap_err().to_string();
        assert!(refused.contains("--prompt"));
    }
}
