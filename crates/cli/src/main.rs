//! Runs sstack from a terminal, interactively or for a single prompt.

mod rebuild;
mod repl;
mod terminal;

use std::error::Error;
use std::io::{self, Write, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use repl::{Exit, Repl, outcome_text};
use sstack_core::agent::{Agent, AgentConfig, TurnContext, TurnEnd};
use sstack_core::config::{Config, Connection};
use sstack_core::credentials::{SystemKeyring, resolve_secret};
use sstack_core::session::{Entry, SessionLog};
use sstack_core::tools::project::Project;
use sstack_core::tools::{Approver, DenyAll, ToolBox};
use sstack_core::workspace;
use terminal::{InputLines, TerminalApprover, render_event};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

/// A coding agent that improves itself.
#[derive(Debug, Parser)]
struct Args {
    /// Run one prompt without interaction. Shell commands are denied.
    #[arg(short, long)]
    prompt: Option<String>,
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

/// The `--resume` value that means the newest Session.
const LATEST: &str = "latest";
/// Keeps the harness's files apart from others in the user's shared directories.
const DIRECTORY_NAME: &str = "sstack";

/// Any failure that ends a run, shown to the person as text.
type AnyError = Box<dyn Error + Send + Sync>;

/// Returns where the harness keeps its files within `base`, failing if `base` is unknown.
fn app_dir(base: Option<PathBuf>) -> Result<PathBuf, AnyError> {
    let base = base.ok_or("cannot find the user data directory")?;
    Ok(base.join(DIRECTORY_NAME))
}

/// Gives this run its Session, resuming one if `resume` asks for it.
fn open_session(
    directory: &Path,
    resume: Option<&str>,
) -> Result<(PathBuf, SessionLog, Vec<Entry>), AnyError> {
    let Some(path) = resume_path(directory, resume)? else {
        let path = workspace::new_session_path(directory);
        let log = SessionLog::create(&path)?;
        return Ok((path, log, Vec::new()));
    };
    let (log, entries) = SessionLog::open(&path)?;
    Ok((path, log, entries))
}

/// Finds the Session to resume, returning nothing if this run starts a new one.
fn resume_path(directory: &Path, resume: Option<&str>) -> Result<Option<PathBuf>, AnyError> {
    match resume {
        None => Ok(None),
        Some(LATEST) => {
            let latest = workspace::latest_session(directory)?;
            latest
                .map(Some)
                .ok_or_else(|| "no session to resume".into())
        }
        Some(id) => Ok(Some(workspace::session_path(directory, id))),
    }
}

/// A harness ready to take input.
struct Run {
    /// The agent running the Session.
    agent: Agent,
    /// Where the Session is stored, which identifies it across a relaunch.
    session: PathBuf,
}

/// Readies a harness for the current directory, as the run's arguments ask.
fn prepare(args: &Args, approver: Arc<dyn Approver>) -> Result<Run, AnyError> {
    let config = load_settings()?;
    let connection = config.connection(args.connection.as_deref())?;
    let project = Arc::new(Project::new(&std::env::current_dir()?)?);
    let backend = Backend {
        connection,
        model: args.model.as_ref().unwrap_or(&config.default_model).clone(),
    };
    let agent_config = agent_config(&backend, project.clone(), approver)?;
    start_session(agent_config, &project, args.resume.as_deref())
}

/// Returns the user's settings.
fn load_settings() -> Result<Config, AnyError> {
    let path = app_dir(dirs::config_dir())?.join("config.toml");
    Ok(Config::load(&path)?)
}

/// The model a run has chosen, and how to reach it.
struct Backend<'a> {
    /// How to reach the model's Provider.
    connection: &'a Connection,
    /// The identifier of the model to request.
    model: String,
}

/// Decides what the agent runs on for a given model and project.
fn agent_config(
    backend: &Backend<'_>,
    project: Arc<Project>,
    approver: Arc<dyn Approver>,
) -> Result<AgentConfig, AnyError> {
    let connection = backend.connection;
    SystemKeyring::connect();
    let secret = resolve_secret(&connection.secret, &SystemKeyring, project.root())?;
    Ok(AgentConfig {
        provider: connection.provider(secret)?,
        model: backend.model.clone(),
        system: workspace::system_prompt(project.root()),
        tools: ToolBox::standard(project, approver, vec![connection.secret.clone()]),
        context_limit: connection.context_limit,
    })
}

/// Opens the Session for this run and starts an agent on it.
fn start_session(
    config: AgentConfig,
    project: &Project,
    resume: Option<&str>,
) -> Result<Run, AnyError> {
    let sessions = workspace::sessions_dir(&app_dir(dirs::data_dir())?, project.root());
    let (session, log, entries) = open_session(&sessions, resume)?;
    let agent = Agent::new(config, log, entries)?;
    Ok(Run { agent, session })
}

/// Arguments that bring a rebuilt harness back to the same Session.
fn relaunch_args(args: &Args, session: &Path) -> Vec<String> {
    let mut relaunch = vec!["--resume".to_owned(), workspace::session_id(session)];
    let options = [("--connection", &args.connection), ("--model", &args.model)];
    for (flag, value) in options {
        if let Some(value) = value {
            relaunch.extend([flag.to_owned(), value.clone()]);
        }
    }
    relaunch
}

/// Runs one prompt with no interaction, denying all shell commands.
async fn run_headless(args: &Args, prompt: &str) -> Result<ExitCode, AnyError> {
    let Run { mut agent, session } = prepare(args, Arc::new(DenyAll))?;
    let cancel = CancellationToken::new();
    let mut out = stdout();
    let mut context = TurnContext {
        cancel: &cancel,
        on_event: &mut |event| render_event(&event, &mut out),
    };
    let result = agent.run_turn(prompt, &mut context).await;
    print!(
        "{}",
        outcome_text(result.as_ref().map(|end| *end).map_err(ToString::to_string))
    );
    println!("\n[session {}]", workspace::session_id(&session));
    Ok(exit_code(result.ok()))
}

/// Returns a success exit code only if the Turn finished normally.
fn exit_code(end: Option<TurnEnd>) -> ExitCode {
    match end {
        Some(TurnEnd::Finished) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Makes typed lines available without blocking the rest of the harness.
fn spawn_input() -> InputLines {
    let (sender, receiver) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if sender.send(line).await.is_err() {
                break;
            }
        }
    });
    Arc::new(Mutex::new(receiver))
}

/// Makes Ctrl-C presses available as messages, one for each press.
fn spawn_interrupts() -> mpsc::Receiver<()> {
    let (sender, receiver) = mpsc::channel(4);
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            let _ = sender.try_send(());
        }
    });
    receiver
}

/// Runs the prompt loop until the person quits, rebuilding on request.
async fn run_interactive(args: &Args) -> Result<ExitCode, AnyError> {
    let input = spawn_input();
    let approver = TerminalApprover::new(input.clone(), std::io::stderr());
    let Run { mut agent, session } = prepare(args, Arc::new(approver))?;
    println!(
        "Session {}. /rebuild, /quit.",
        workspace::session_id(&session)
    );
    let mut repl = Repl {
        input,
        interrupts: spawn_interrupts(),
        out: stdout(),
    };
    while repl.run(&mut agent).await == Exit::Rebuild {
        let rebuilder = Rebuilder {
            build: rebuild::build,
            relaunch: rebuild::relaunch,
        };
        rebuilder.run(&relaunch_args(args, &session), &mut repl);
    }
    Ok(ExitCode::SUCCESS)
}

/// The two steps of a rebuild, kept apart so that the decision between them can be tested.
struct Rebuilder<B, R> {
    /// Builds the Harness Source, returning the new binary if that worked.
    build: B,
    /// Replaces this process with a binary, returning only if that fails.
    relaunch: R,
}

impl<B, R> Rebuilder<B, R>
where
    B: FnOnce() -> Option<PathBuf>,
    R: FnOnce(&Path, &[String]) -> io::Error,
{
    /// Replaces the running harness with a fresh build, passing it `relaunch_args`.
    ///
    /// Only returns if that could not be done.
    fn run<W: Write + Send>(self, relaunch_args: &[String], repl: &mut Repl<W>) {
        match (self.build)() {
            Some(binary) => {
                let error = (self.relaunch)(&binary, relaunch_args);
                repl.report_rebuild_failure(error);
            }
            None => repl.report_rebuild_failure("the build did not succeed"),
        }
    }
}

/// Runs the mode the arguments ask for.
async fn run(args: Args) -> Result<ExitCode, AnyError> {
    match args.prompt.as_deref() {
        Some(prompt) => run_headless(&args, prompt).await,
        None => run_interactive(&args).await,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    report(run(Args::parse()).await)
}

/// Prints a failure and returns the matching exit code.
fn report(result: Result<ExitCode, AnyError>) -> ExitCode {
    result.unwrap_or_else(|error| {
        eprintln!("error: {error}");
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cli: &[&str]) -> Args {
        Args::parse_from(std::iter::once("sstack").chain(cli.iter().copied()))
    }

    #[test]
    fn resume_accepts_an_id_or_means_the_newest() {
        assert_eq!(args(&["--resume"]).resume.as_deref(), Some(LATEST));
        assert_eq!(args(&["--resume", "12"]).resume.as_deref(), Some("12"));
        assert_eq!(args(&[]).resume, None);
    }

    #[test]
    fn a_new_session_is_created_and_can_be_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut log, entries) = open_session(dir.path(), None).unwrap();
        assert_eq!(entries, Vec::new());
        log.append(&Entry::TurnCancelled).unwrap();

        let id = workspace::session_id(&path);
        let (by_id, _, entries) = open_session(dir.path(), Some(&id)).unwrap();
        let (newest, _, _) = open_session(dir.path(), Some(LATEST)).unwrap();

        assert_eq!(by_id, path);
        assert_eq!(newest, path);
        assert_eq!(entries, [Entry::TurnCancelled]);
    }

    #[test]
    fn resuming_with_nothing_to_resume_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(open_session(dir.path(), Some(LATEST)).is_err());
        assert!(open_session(dir.path(), Some("missing")).is_err());
    }

    #[test]
    fn relaunching_keeps_the_session_and_chosen_options() {
        let plain = relaunch_args(&args(&[]), Path::new("/s/123.jsonl"));
        assert_eq!(plain, ["--resume", "123"]);
        let chosen = args(&["--connection", "work", "--model", "m"]);
        assert_eq!(
            relaunch_args(&chosen, Path::new("/s/123.jsonl")),
            ["--resume", "123", "--connection", "work", "--model", "m"]
        );
    }

    #[test]
    fn only_a_finished_turn_is_a_success() {
        let code = |end| format!("{:?}", exit_code(end));
        assert_eq!(
            code(Some(TurnEnd::Finished)),
            format!("{:?}", ExitCode::SUCCESS)
        );
        assert_eq!(
            code(Some(TurnEnd::Refused)),
            format!("{:?}", ExitCode::FAILURE)
        );
        assert_eq!(code(None), format!("{:?}", ExitCode::FAILURE));
    }

    fn repl() -> Repl<Vec<u8>> {
        let (_, input) = mpsc::channel(1);
        let (_, interrupts) = mpsc::channel(1);
        Repl {
            input: Arc::new(Mutex::new(input)),
            interrupts,
            out: Vec::new(),
        }
    }

    #[test]
    fn a_failed_build_leaves_the_harness_running_and_says_so() {
        let mut repl = repl();
        let rebuilder = Rebuilder {
            build: || None,
            relaunch: |_: &Path, _: &[String]| panic!("a failed build must not relaunch"),
        };
        rebuilder.run(&[], &mut repl);
        let shown = String::from_utf8(repl.out).unwrap();
        assert!(shown.contains("the build did not succeed"));
    }

    #[test]
    fn a_successful_build_relaunches_with_the_given_arguments() {
        let mut repl = repl();
        let mut launched = None;
        let rebuilder = Rebuilder {
            build: || Some(PathBuf::from("/bin/sstack")),
            relaunch: |binary: &Path, arguments: &[String]| {
                launched = Some((binary.to_owned(), arguments.to_vec()));
                io::Error::other("exec failed")
            },
        };
        rebuilder.run(&["--resume".to_owned()], &mut repl);
        let (binary, arguments) = launched.unwrap();
        assert_eq!(binary, Path::new("/bin/sstack"));
        assert_eq!(arguments, ["--resume"]);
        assert!(String::from_utf8(repl.out).unwrap().contains("exec failed"));
    }

    #[test]
    fn the_app_directory_needs_a_base() {
        assert!(app_dir(None).is_err());
        let found = app_dir(Some(PathBuf::from("/base"))).unwrap();
        assert_eq!(found, Path::new("/base/sstack"));
    }

    #[test]
    fn failures_exit_unsuccessfully() {
        let failure = format!("{:?}", ExitCode::FAILURE);
        assert_eq!(format!("{:?}", report(Err("boom".into()))), failure);
    }
}
