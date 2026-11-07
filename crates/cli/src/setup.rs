//! Readying an agent and its Session for either mode, as the run's arguments ask.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use bon::builder;
use sstack::agent::{Agent, AgentConfig};
use sstack::config::{Config, Connection};
use sstack::credentials::{SystemKeyring, resolve_secret};
use sstack::session::{Entry, SessionLog};
use sstack::skills::{self, Skill};
use sstack::tools::project::Project;
use sstack::tools::{Approver, Land, ToolBox};
use sstack::workspace::{self, SkillListing};
use sstack::worktree;

use crate::{Args, LATEST, describe};

/// Keeps the harness's files apart from others in the user's shared directories.
const DIRECTORY_NAME: &str = "sstack";

/// Returns where the harness keeps its files within `base`, failing if `base` is unknown.
fn app_dir(base: Option<PathBuf>) -> Result<PathBuf> {
    let base = base.context("cannot find the user data directory")?;
    Ok(base.join(DIRECTORY_NAME))
}

/// Where a new Session works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// In a worktree of its own, away from other Sessions.
    Worktree,
    /// In the main checkout, shared with anything else working there.
    MainCheckout,
}

/// Which Session a run works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start<'a> {
    /// The Session with this id, or the newest one for [`LATEST`], archived or not.
    Resume(&'a str),
    /// A new Session in this place.
    New(Place),
}

/// A Session log that is open and ready for an agent.
struct OpenedLog {
    /// Where the Session is stored.
    path: PathBuf,
    /// Records the Session.
    log: SessionLog,
    /// The Session so far.
    entries: Vec<Entry>,
    /// Whether the Session should get a worktree if it has none.
    wants_worktree: bool,
}

/// Opens the log of the Session that `start` asks for.
fn open_session(directory: &Path, start: Start<'_>) -> Result<OpenedLog> {
    match start {
        Start::New(place) => {
            let path = workspace::new_session_path(directory);
            let log = SessionLog::create(&path)?;
            Ok(OpenedLog {
                path,
                log,
                entries: Vec::new(),
                wants_worktree: place == Place::Worktree,
            })
        }
        Start::Resume(wanted) => resume_session(directory, wanted),
    }
}

/// Opens the log of an existing Session, bringing it back first if it was archived.
fn resume_session(directory: &Path, wanted: &str) -> Result<OpenedLog> {
    let wants_worktree = workspace::restore_session(directory, wanted)?;
    let path = resume_path(directory, wanted)?;
    let (log, entries) = SessionLog::open(&path)?;
    Ok(OpenedLog {
        path,
        log,
        entries,
        wants_worktree,
    })
}

/// Finds the Session to resume.
fn resume_path(directory: &Path, wanted: &str) -> Result<PathBuf> {
    if wanted == LATEST {
        let latest = workspace::latest_session(directory)?;
        latest.context("no session to resume")
    } else {
        Ok(workspace::session_path(directory, wanted))
    }
}

/// A harness ready to take input.
pub struct Run {
    /// The agent running the Session.
    pub agent: Agent,
    /// Where the Session is stored, which identifies it across a relaunch.
    pub session: PathBuf,
    /// Things the person should know about how the Session was set up, in the order they
    /// happened.
    pub notices: Vec<String>,
    /// Whether the person wants to type into a real nvim.
    pub vim: bool,
}

/// Where a Session turned out to work.
struct Workplace {
    /// The Session's worktree. Missing for a Session in the main checkout.
    worktree: Option<PathBuf>,
    /// Something the person should know about how that was decided.
    notice: Option<String>,
}

/// Finds or makes the place for the Session with id `id` to work. A project that cannot have
/// worktrees still works, in the main checkout.
fn workplace(main: &Path, id: &str, wants_worktree: bool) -> Workplace {
    let existing = worktree::path(main, id);
    let (worktree, notice) = if existing.exists() {
        (Some(existing), None)
    } else if !wants_worktree {
        (None, None)
    } else {
        match worktree::create(main, id) {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(format!("working in the main checkout: {error}"))),
        }
    };
    Workplace { worktree, notice }
}

/// Readies a harness for the current directory, with the [`Connection`] and model the run's
/// arguments ask for.
///
/// `start` says which Session to work on. `approver` is given the Session's id and returns what
/// decides whether each shell command the model asks for may run.
pub fn prepare(
    args: &Args,
    start: Start<'_>,
    approver: impl FnOnce(&str) -> Arc<dyn Approver>,
) -> Result<Run> {
    let config = load_settings()?;
    let connection = config.connection(args.connection.as_deref())?;
    let main = main_checkout()?;
    let backend = Backend {
        connection,
        model: args.model.as_ref().unwrap_or(&config.default_model).clone(),
        main: &main,
    };
    let session = open_session(&project_sessions(&main)?, start)?;
    let id = workspace::session_id(&session.path);
    let place = workplace(&main, &id, session.wants_worktree);
    let offered = offered_skills(place.worktree.as_deref().unwrap_or(&main));
    let listing = SkillListing::new(&offered, connection.context_limit);
    let agent_config = agent_config()
        .backend(&backend)
        .id(&id)
        .place(&place)
        .approver(approver(&id))
        .skills(offered)
        .listing(&listing)
        .call()?;
    let shortened = describe::listing_shortened(listing.without_descriptions);
    Ok(Run {
        agent: Agent::new(agent_config, session.log, session.entries)?,
        session: session.path,
        notices: place.notice.into_iter().chain(shortened).collect(),
        vim: config.vim,
    })
}

/// Returns the directory the harness was started in, with symlinks resolved.
pub fn main_checkout() -> Result<PathBuf> {
    Ok(Project::new(&std::env::current_dir()?)?.root().to_owned())
}

/// Returns where the Sessions of the current directory are stored.
pub fn sessions_directory() -> Result<PathBuf> {
    project_sessions(&main_checkout()?)
}

/// Returns where the Sessions of the project at `root` are stored.
fn project_sessions(root: &Path) -> Result<PathBuf> {
    Ok(workspace::sessions_dir(&app_dir(dirs::data_dir())?, root))
}

/// Returns the user's settings.
fn load_settings() -> Result<Config> {
    let path = app_dir(dirs::config_dir())?.join("config.toml");
    Ok(Config::load(&path)?)
}

/// The model a run has chosen, and how to reach it.
struct Backend<'a> {
    /// How to reach the model's Provider.
    connection: &'a Connection,
    /// The identifier of the model to request.
    model: String,
    /// The main checkout, where a `.env` file would be, since git leaves it out of worktrees.
    main: &'a Path,
}

/// Finds the [`Skill`]s for a Session working in the project at `root`: the project's own,
/// including those of the directory the person started in if it lies inside `root`, and the
/// person's.
pub fn installed_skills(root: &Path) -> Vec<Skill> {
    let home = dirs::home_dir();
    let start = std::env::current_dir().unwrap_or_else(|_| root.to_owned());
    skills::discover(&skills::locations(root, &start, home.as_deref()))
}

/// Returns the [`Skill`]s the model may load on its own in a Session working in the project at
/// `root`.
fn offered_skills(root: &Path) -> Vec<Skill> {
    installed_skills(root)
        .into_iter()
        .filter(|skill| skill.model_invocable)
        .collect()
}

/// Decides what the agent for a Session runs on.
#[builder]
fn agent_config(
    /// The model the run has chosen.
    backend: &Backend<'_>,
    /// The id of the Session.
    id: &str,
    /// Where the Session works.
    place: &Workplace,
    /// Decides on the Session's shell commands.
    approver: Arc<dyn Approver>,
    /// The Skills the model may load.
    skills: Vec<Skill>,
    /// How the system prompt lists those Skills.
    listing: &SkillListing,
) -> Result<AgentConfig> {
    let connection = backend.connection;
    SystemKeyring::connect();
    let secret = resolve_secret(&connection.secret, &SystemKeyring, backend.main)?;
    let root = place.worktree.as_deref().unwrap_or(backend.main);
    let skill_directories = skills
        .iter()
        .filter_map(|s| s.path.parent().map(Path::to_owned));
    let project = Arc::new(Project::new(root)?.with_readable(skill_directories));
    let secret_env = vec![connection.secret.clone()];
    let tools = ToolBox::standard(project.clone(), approver.clone(), secret_env.clone())
        .with_skills(skills);
    let home = dirs::home_dir();
    let claude = skills::claude_directory(home.as_deref(), |name| std::env::var_os(name));
    let system = workspace::system_prompt(root, claude.as_deref());
    let system = workspace::skills_prompt(&system, listing);
    let (tools, system) = if place.worktree.is_some() {
        let land = Land {
            worktree: project,
            main: backend.main.to_owned(),
            branch: worktree::branch(id),
            approver,
            secret_env,
        };
        (tools.with_land(land), workspace::worktree_prompt(&system))
    } else {
        (tools, system)
    };
    Ok(AgentConfig {
        provider: connection.provider(secret)?,
        model: backend.model.clone(),
        system,
        tools,
        context_limit: connection.context_limit,
    })
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
        let mut new = open_session(dir.path(), Start::New(Place::Worktree)).unwrap();
        assert_eq!(new.entries, Vec::new());
        assert!(new.wants_worktree);
        new.log.append(&Entry::TurnCancelled).unwrap();

        let id = workspace::session_id(&new.path);
        let by_id = open_session(dir.path(), Start::Resume(&id)).unwrap();
        let newest = open_session(dir.path(), Start::Resume(LATEST)).unwrap();

        assert_eq!(by_id.path, new.path);
        assert_eq!(newest.path, new.path);
        assert_eq!(by_id.entries, [Entry::TurnCancelled]);
        assert!(!by_id.wants_worktree);
    }

    #[test]
    fn resuming_with_nothing_to_resume_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(open_session(dir.path(), Start::Resume(LATEST)).is_err());
        assert!(open_session(dir.path(), Start::Resume("missing")).is_err());
    }

    #[test]
    fn a_main_checkout_session_wants_no_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let new = open_session(dir.path(), Start::New(Place::MainCheckout)).unwrap();
        assert!(!new.wants_worktree);
    }

    #[test]
    fn resuming_an_archived_session_restores_it_and_asks_for_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let path = workspace::session_path(dir.path(), "1");
        SessionLog::create(&path).unwrap();
        workspace::archive_session(dir.path(), "1").unwrap();

        let restored = open_session(dir.path(), Start::Resume("1")).unwrap();

        assert_eq!(restored.path, path);
        assert!(restored.wants_worktree);
    }

    #[test]
    fn sessions_work_where_they_can() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path();

        let plain = workplace(main, "1", false);
        assert_eq!((plain.worktree, plain.notice), (None, None));

        let fallback = workplace(main, "1", true);
        assert_eq!(fallback.worktree, None);
        assert!(fallback.notice.unwrap().contains("main checkout"));

        let existing = worktree::path(main, "2");
        std::fs::create_dir_all(&existing).unwrap();
        let found = workplace(main, "2", false);
        assert_eq!(found.worktree, Some(existing));
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
