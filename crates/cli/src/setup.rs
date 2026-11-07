//! Readying an agent and its Session for either mode, as the run's arguments ask.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sstack_core::agent::{Agent, AgentConfig};
use sstack_core::config::{Config, Connection};
use sstack_core::credentials::{SystemKeyring, resolve_secret};
use sstack_core::session::{Entry, SessionLog};
use sstack_core::tools::project::Project;
use sstack_core::tools::{Approver, ToolBox};
use sstack_core::workspace;

use crate::{AnyError, Args, LATEST};

/// Keeps the harness's files apart from others in the user's shared directories.
const DIRECTORY_NAME: &str = "sstack";

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
pub struct Run {
    /// The agent running the Session.
    pub agent: Agent,
    /// Where the Session is stored, which identifies it across a relaunch.
    pub session: PathBuf,
}

/// Readies a harness for the current directory, with the Connection and model the run's
/// arguments ask for.
///
/// `resume` names the Session to resume, as `--resume` does, and a new one is started without
/// it. `approver` decides whether each shell command the model asks for may run.
pub fn prepare(
    args: &Args,
    resume: Option<&str>,
    approver: Arc<dyn Approver>,
) -> Result<Run, AnyError> {
    let config = load_settings()?;
    let connection = config.connection(args.connection.as_deref())?;
    let project = Arc::new(Project::new(&std::env::current_dir()?)?);
    let backend = Backend {
        connection,
        model: args.model.as_ref().unwrap_or(&config.default_model).clone(),
    };
    let agent_config = agent_config(&backend, project.clone(), approver)?;
    start_session(agent_config, &project, resume)
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

/// Returns the arguments that bring a rebuilt harness back to the same Session.
pub fn relaunch_args(args: &Args, session: &Path) -> Vec<String> {
    let mut relaunch = vec!["--resume".to_owned(), workspace::session_id(session)];
    let options = [("--connection", &args.connection), ("--model", &args.model)];
    for (flag, value) in options {
        if let Some(value) = value {
            relaunch.extend([flag.to_owned(), value.clone()]);
        }
    }
    relaunch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::args;

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
    fn the_app_directory_needs_a_base() {
        assert!(app_dir(None).is_err());
        let found = app_dir(Some(PathBuf::from("/base"))).unwrap();
        assert_eq!(found, Path::new("/base/sstack"));
    }
}
