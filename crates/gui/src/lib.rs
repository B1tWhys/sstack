//! The windowed front end of sstack, which opens the Sessions of one project in one window.
//!
//! Sessions, Providers, tools and the agent loop are in [`sstack`]. The window turns what the
//! person does into calls on core and draws what core reports about the Session.

mod asked;
mod hint;
mod inbox;
mod reports;
#[cfg(test)]
mod test_support;
mod theme;
mod transcript;
mod vim_prompt;
mod window;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use iced::futures::future::{BoxFuture, FutureExt};
use iced::{Application, Program, Task};
use reports::Reports;
use sstack::question::Asker;
use sstack::rebuild::{self, Binary, Checkout};
use sstack::sandbox::Containers;
use sstack::session_report::{ReportingApprover, ReportingAsker};
use sstack::session_setup::{
    AskedConnection, LATEST, Start, listed_sessions, main_checkout, prepare, sessions_directory,
    starting_session,
};
use sstack::{workspace, worktree};
use window::{Message, Opened, Ready, Sessions, Window};

// The flags match the TUI's, except `--prompt`, but are defined apart from the CLI's, because
// the GUI is free to change them, for instance once it can choose a project or a Session itself.

/// A coding agent that improves itself, worked with in a window.
#[derive(Debug, Parser)]
#[command(name = "sstack-gui")]
struct Args {
    /// Connection to run on, instead of the one the Session last ran on or the default in
    /// settings. Sessions started in this run use it too, and each keeps it.
    #[arg(long)]
    connection: Option<String>,
    /// Model to use, instead of the one the Connection names in settings.
    #[arg(long)]
    model: Option<String>,
    /// Resume a Session by id, or the newest one if no id is given.
    #[arg(long, num_args = 0..=1, default_missing_value = LATEST)]
    resume: Option<String>,
}

impl Args {
    /// Returns the arguments that bring a rebuilt `sstack-gui` back to the Session stored at
    /// `session`, with the same model as this run. Each Session keeps its own Connection, so the
    /// Connection this run was asked for is not passed on.
    ///
    /// The CLI's `Args::relaunch` does the same for its flags. The two are kept apart for the
    /// reason the flags are.
    fn relaunch(&self, session: &Path) -> Vec<String> {
        let mut relaunch = vec!["--resume".to_owned(), workspace::session_id(session)];
        if let Some(model) = &self.model {
            relaunch.extend(["--model".to_owned(), model.clone()]);
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

/// Opens a window on the project of the current directory, showing the Session the arguments
/// ask for, which stays open until the person closes it, quits or rebuilds. After a Rebuild, the
/// process becomes the new `sstack-gui` on the Session that was on screen, once iced has torn
/// down the window.
///
/// A first Session that cannot be opened fails here, before any window appears, so the person
/// reads why in the terminal they started from. So does a relaunch that fails, since the window
/// is gone by then.
fn run(args: &Args) -> Result<()> {
    let ready = ready(args)?;
    let rebuilt = Arc::clone(&ready.relaunch);
    application(ready).run()?;
    rebuilt.get().map_or(Ok(()), |rebuilt| {
        Err(relaunch(&rebuilt.binary, &args.relaunch(&rebuilt.session)))
    })
}

/// Replaces this process with `binary`, started with `args`, returning why it could not.
fn relaunch(binary: &Path, args: &[String]) -> anyhow::Error {
    let failure = rebuild::relaunch(binary, args);
    anyhow!("could not relaunch into {}: {failure}", binary.display())
}

/// Returns the GUI as iced runs it, with one window on the readied Sessions. Closing the window
/// quits as `/quit` does, rather than ending a running Turn wherever it is.
fn application(ready: Ready) -> Application<impl Program<State = Window, Message = Message>> {
    let unopened = Cell::new(Some(ready));
    iced::application(move || open(&unopened), Window::update, Window::view)
        .title(Window::title)
        .theme(theme::theme())
        .subscription(Window::subscription)
        .font(lucide_icons::LUCIDE_FONT_BYTES)
        .exit_on_close_request(false)
}

/// Readies the window on the project of the current directory, with the Session the arguments
/// ask for to show first and the project's stored Sessions to list.
fn ready(args: &Args) -> Result<Ready> {
    let started_in = std::env::current_dir()?;
    let main = main_checkout(&started_in)?;
    let mut sessions = LiveSessions {
        connection: AskedConnection::new(args.connection.clone()),
        model: args.model.clone(),
        directory: sessions_directory(&started_in)?,
        main,
        started_in,
        containers: Arc::new(Containers::detect()),
    };
    let stored = listed_sessions(&sessions.main, &sessions.directory)?;
    let first = sessions.open(starting_session(args.resume.as_deref(), &stored))?;
    Ok(Ready {
        first,
        stored,
        sessions: Box::new(sessions),
        build: build_harness,
        relaunch: Arc::default(),
    })
}

/// Real Sessions of one project, opened, archived and renamed on behalf of the window.
struct LiveSessions {
    /// The Connection this run was asked to use, and which Sessions it applies to.
    connection: AskedConnection,
    /// The model every Session uses, instead of the one its Connection names in settings.
    model: Option<String>,
    /// The main checkout of the project.
    main: PathBuf,
    /// The directory the person started `sstack-gui` in, inside the project.
    started_in: PathBuf,
    /// Where the project's Sessions are stored.
    directory: PathBuf,
    /// The container runtime shared by every Session's Sandbox, so that the runtime is chosen and
    /// the image built once.
    containers: Arc<Containers>,
}

impl Sessions for LiveSessions {
    /// Readies the Session `start` asks for. Shell commands that the Sandbox cannot run are asked
    /// about in the window, and the model's Questions are put to the person there.
    fn open(&mut self, start: Start<'_>) -> Result<Opened> {
        let (reports, sender) = Reports::new();
        let approver = Arc::new(ReportingApprover::new(sender.clone()));
        let asker: Arc<dyn Asker> = Arc::new(ReportingAsker::new(sender.clone()));
        let run = prepare()
            .start(start)
            .directory(&self.started_in)
            .maybe_connection(self.connection.for_opening(start))
            .maybe_model(self.model.as_deref())
            .approver(|_| approver)
            .asker(|_| Some(asker))
            .containers(Arc::clone(&self.containers))
            .call()?;
        Ok(Opened {
            run,
            reports,
            sender,
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

    fn harness_source(&self, checkout: Checkout, id: &str) -> Result<PathBuf, String> {
        rebuild::source(checkout, &worktree::path(&self.main, id))
    }
}

/// Builds `sstack-gui` from the checkout of the Harness Source at `source` for a Rebuild, ending
/// in where the new binary is or in why the build failed.
fn build_harness(source: PathBuf) -> BoxFuture<'static, Result<PathBuf, String>> {
    rebuild::build(HARNESS, source).boxed()
}

/// Opens the window on the readied Sessions, which iced asks for once as it starts, along with
/// the task that maximizes the window.
///
/// # Panics
///
/// Panics if asked a second time, since the Sessions can be opened only once.
fn open(unopened: &Cell<Option<Ready>>) -> (Window, Task<Message>) {
    let window = Window::open(unopened.take().expect("iced opens the window once"));
    (window, maximize())
}

/// Returns the task that maximizes the newest window, so that it fills the screen but stays an
/// ordinary window rather than taking a full-screen space of its own.
///
/// It must run once the window is showing. macOS ignores a request to maximize a hidden window,
/// and iced creates its window hidden, so `maximized` in the window's settings has no effect
/// there. iced runs the task its startup returns only once the window is showing.
fn maximize() -> Task<Message> {
    iced::window::latest().and_then(|id| iced::window::maximize(id, true))
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
    use iced::futures::StreamExt;
    use iced::futures::channel::mpsc;
    use iced_test::emulator::{Emulator, Event, Mode};
    use iced_test::runtime::{self, window::Action as WindowAction};
    use iced_test::simulator;
    use tempfile::TempDir;

    use crate::test_support::{bash_call, readied, typed};

    /// Returns whether the window that `emulator` runs shows `text`.
    fn shows<P: Program + 'static>(emulator: &Emulator<P>, program: &P, text: &str) -> bool {
        simulator(emulator.view(program)).find(text).is_ok()
    }

    #[test]
    fn reports_reach_the_window_through_its_subscription() {
        let dir = TempDir::new().unwrap();
        // A reply is drawn as Markdown, which tests cannot find, so this waits for the question
        // whether a command may run, which shows the command as plain text.
        let script = vec![Ok(bash_call("touch ran"))];
        let program = application(readied(dir.path(), script, &[]));
        let (sender, mut events) = mpsc::channel(100);
        let size = Size::new(800.0, 600.0);
        let mut emulator = Emulator::new(sender, &program, Mode::Immediate, size);

        for message in typed("hello").into_iter().chain([Message::Submit]) {
            emulator.update(&program, message);
        }

        // Without the question, this never ends, and nextest stops the test.
        loop {
            while let Ok(event) = events.try_recv() {
                if let Event::Action(action) = event {
                    emulator.perform(&program, action);
                }
            }
            if shows(&emulator, &program, "touch ran") {
                break;
            }
        }
    }

    #[tokio::test]
    async fn opening_maximizes_the_newest_window() {
        let mut actions = runtime::task::into_stream(maximize()).unwrap();
        let Some(runtime::Action::Window(WindowAction::GetLatest(latest))) = actions.next().await
        else {
            panic!("the task should ask which window is newest");
        };
        let newest = iced::window::Id::unique();
        latest.send(Some(newest)).unwrap();

        let maximized = actions.next().await;

        assert!(
            matches!(
                maximized,
                Some(runtime::Action::Window(WindowAction::Maximize(id, true))) if id == newest
            ),
            "the task should maximize the newest window"
        );
    }

    /// Parses `cli` as if it followed the program name on the command line.
    fn args(cli: &[&str]) -> Args {
        Args::parse_from(std::iter::once("sstack-gui").chain(cli.iter().copied()))
    }

    #[test]
    fn the_connection_and_model_can_be_chosen() {
        let chosen = args(&["--connection", "work", "--model", "m"]);
        assert_eq!(chosen.connection.as_deref(), Some("work"));
        assert_eq!(chosen.model.as_deref(), Some("m"));
    }

    #[test]
    fn relaunching_keeps_the_session_and_model_but_not_the_connection() {
        let plain = args(&[]).relaunch(Path::new("/s/123.jsonl"));
        assert_eq!(plain, ["--resume", "123"]);
        let chosen = args(&["--connection", "work", "--model", "m", "--resume", "9"]);
        assert_eq!(
            chosen.relaunch(Path::new("/s/123.jsonl")),
            ["--resume", "123", "--model", "m"]
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
