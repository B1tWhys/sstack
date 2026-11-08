//! The windowed front end of sstack, which opens one Session in one window.
//!
//! Sessions, Providers, tools and the agent loop are in [`sstack`]. The window turns what the
//! person does into calls on core and draws what core reports about the Session.

mod reports;
#[cfg(test)]
mod test_support;
mod transcript;
mod window;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use clap::Parser;
use iced::futures::future::{BoxFuture, FutureExt};
use iced::{Application, Program};
use reports::Reports;
use sstack::rebuild::{self, Binary};
use sstack::sandbox::Containers;
use sstack::session_report::ReportingApprover;
use sstack::session_setup::{LATEST, Place, Start, prepare};
use sstack::workspace;
use window::{Message, Ready, Window};

// The flags match the TUI's, except `--prompt`, but are defined apart from the CLI's, because
// the GUI is free to change them, for instance once it can choose a project or a Session itself.

/// A coding agent that improves itself, worked with in a window.
#[derive(Debug, Parser)]
#[command(name = "sstack-gui")]
struct Args {
    /// Connection to use, instead of the default in settings.
    #[arg(long)]
    connection: Option<String>,
    /// Model to use, instead of the default in settings.
    #[arg(long)]
    model: Option<String>,
    /// Resume a Session by id, or the newest one if no id is given.
    #[arg(long, num_args = 0..=1, default_missing_value = LATEST)]
    resume: Option<String>,
}

impl Args {
    /// Returns which Session to open: the one `--resume` names, or else a new one in its own
    /// worktree.
    fn start(&self) -> Start<'_> {
        self.resume
            .as_deref()
            .map_or(Start::New(Place::Worktree), Start::Resume)
    }

    /// Returns the arguments that bring a rebuilt `sstack-gui` back to the Session stored at
    /// `session`, with the same Connection and model as this run.
    ///
    /// The CLI's `Args::relaunch` does the same for its flags. The two are kept apart for the
    /// reason the flags are.
    fn relaunch(&self, session: &Path) -> Vec<String> {
        let mut relaunch = vec!["--resume".to_owned(), workspace::session_id(session)];
        let options = [("--connection", &self.connection), ("--model", &self.model)];
        for (flag, value) in options {
            if let Some(value) = value {
                relaunch.extend([flag.to_owned(), value.clone()]);
            }
        }
        relaunch
    }
}

/// The binary a Rebuild builds and relaunches into, which is this one.
const HARNESS: Binary = Binary {
    package: env!("CARGO_PKG_NAME"),
    name: "sstack-gui",
};

/// Runs the GUI with the process's arguments until its window closes, and reports how it went.
pub fn run_from_command_line() -> ExitCode {
    report(run(&Args::parse()))
}

/// Opens the Session the arguments ask for in a window, which stays open until the person
/// closes it, quits or rebuilds. After a Rebuild, the process becomes the new `sstack-gui` on the
/// same Session, once iced has torn down the window.
///
/// A Session that cannot be opened fails here, before any window appears, so the person reads
/// why in the terminal they started from. So does a relaunch that fails, since the window is
/// gone by then.
fn run(args: &Args) -> Result<()> {
    let ready = ready(args)?;
    let relaunch_args = args.relaunch(&ready.run.session);
    let rebuilt = Arc::clone(&ready.relaunch);
    application(ready).run()?;
    rebuilt
        .get()
        .map_or(Ok(()), |binary| Err(relaunch(binary, &relaunch_args)))
}

/// Replaces this process with `binary`, started with `args`, returning why it could not.
fn relaunch(binary: &Path, args: &[String]) -> anyhow::Error {
    let failure = rebuild::relaunch(binary, args);
    anyhow!("could not relaunch into {}: {failure}", binary.display())
}

/// Returns the GUI as iced runs it, with one window on the readied Session. Closing the window
/// quits as `/quit` does, rather than ending a running Turn wherever it is.
fn application(ready: Ready) -> Application<impl Program<State = Window, Message = Message>> {
    let unopened = Cell::new(Some(ready));
    iced::application(move || open(&unopened), Window::update, Window::view)
        .title(Window::title)
        .subscription(Window::subscription)
        .exit_on_close_request(false)
}

/// Readies the Session the arguments ask for, in the project of the current directory.
///
/// Shell commands that the Sandbox cannot run are asked about in the window.
fn ready(args: &Args) -> Result<Ready> {
    let (reports, sender) = Reports::new();
    let approver = Arc::new(ReportingApprover::new(sender.clone()));
    let run = prepare()
        .start(args.start())
        .directory(&std::env::current_dir()?)
        .maybe_connection(args.connection.as_deref())
        .maybe_model(args.model.as_deref())
        .approver(|_| approver)
        .containers(Arc::new(Containers::detect()))
        .call()?;
    Ok(Ready {
        run,
        reports,
        sender,
        build: build_harness,
        relaunch: Arc::default(),
    })
}

/// Builds `sstack-gui` from the Harness Source for a Rebuild, ending in where the new binary is
/// or in why the build failed.
fn build_harness() -> BoxFuture<'static, Result<PathBuf, String>> {
    rebuild::build(HARNESS).boxed()
}

/// Opens the window on the readied Session, which iced asks for once as it starts.
///
/// # Panics
///
/// Panics if asked a second time, since the Session can be opened only once.
fn open(unopened: &Cell<Option<Ready>>) -> Window {
    Window::open(unopened.take().expect("iced opens the window once"))
}

/// Prints a failure and returns the matching exit code.
fn report(result: Result<()>) -> ExitCode {
    result.map_or_else(
        |error| {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        },
        |()| ExitCode::SUCCESS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use iced::Size;
    use iced::futures::channel::mpsc;
    use iced_test::emulator::{Emulator, Event, Mode};
    use iced_test::simulator;
    use tempfile::TempDir;

    use crate::test_support::{readied, reply, typed};

    /// Returns whether the window that `emulator` runs shows `text`.
    fn shows<P: Program + 'static>(emulator: &Emulator<P>, program: &P, text: &str) -> bool {
        simulator(emulator.view(program)).find(text).is_ok()
    }

    #[test]
    fn reports_reach_the_window_through_its_subscription() {
        let dir = TempDir::new().unwrap();
        let program = application(readied(dir.path(), vec![Ok(reply("hi there"))], &[]));
        let (sender, mut events) = mpsc::channel(100);
        let size = Size::new(800.0, 600.0);
        let mut emulator = Emulator::new(sender, &program, Mode::Immediate, size);

        for message in typed("hello").into_iter().chain([Message::Submit]) {
            emulator.update(&program, message);
        }

        // Without the reply, this never ends, and nextest stops the test.
        loop {
            while let Ok(event) = events.try_recv() {
                if let Event::Action(action) = event {
                    emulator.perform(&program, action);
                }
            }
            if shows(&emulator, &program, "hi there") {
                break;
            }
        }
    }

    /// Parses `cli` as if it followed the program name on the command line.
    fn args(cli: &[&str]) -> Args {
        Args::parse_from(std::iter::once("sstack-gui").chain(cli.iter().copied()))
    }

    #[test]
    fn a_new_session_gets_its_own_worktree_unless_one_is_resumed() {
        assert_eq!(args(&[]).start(), Start::New(Place::Worktree));
        assert_eq!(args(&["--resume"]).start(), Start::Resume(LATEST));
        assert_eq!(args(&["--resume", "12"]).start(), Start::Resume("12"));
    }

    #[test]
    fn the_connection_and_model_can_be_chosen() {
        let chosen = args(&["--connection", "work", "--model", "m"]);
        assert_eq!(chosen.connection.as_deref(), Some("work"));
        assert_eq!(chosen.model.as_deref(), Some("m"));
    }

    #[test]
    fn relaunching_keeps_the_session_and_chosen_options() {
        let plain = args(&[]).relaunch(Path::new("/s/123.jsonl"));
        assert_eq!(plain, ["--resume", "123"]);
        let chosen = args(&["--connection", "work", "--model", "m", "--resume", "9"]);
        assert_eq!(
            chosen.relaunch(Path::new("/s/123.jsonl")),
            ["--resume", "123", "--connection", "work", "--model", "m"]
        );
    }

    #[test]
    fn a_relaunch_that_fails_says_so() {
        let failure = relaunch(Path::new("/definitely/not/here"), &[]).to_string();
        assert!(
            failure.starts_with("could not relaunch into /definitely/not/here: "),
            "{failure}"
        );
    }

    #[test]
    fn failures_exit_unsuccessfully() {
        let failure = format!("{:?}", ExitCode::FAILURE);
        assert_eq!(format!("{:?}", report(Err(anyhow!("boom")))), failure);
        let success = format!("{:?}", ExitCode::SUCCESS);
        assert_eq!(format!("{:?}", report(Ok(()))), success);
    }
}
