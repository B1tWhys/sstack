//! The setup of an agent and its Session, shared by every interface.
//!
//! An interface says which Session to start or resume, which Connection and model to use if not
//! the defaults in settings, and the directory the person started in. Everything else, from the
//! user's settings to the Workspace and the Sandbox, is found from those.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bon::builder;

use crate::agent::{Agent, AgentConfig};
use crate::config::{Config, ConfigError, Connection};
use crate::connections::{self, Connections};
use crate::credentials::{MissingSecret, SystemKeyring, resolve_secret};
use crate::provider::{Provider, ProviderError};
use crate::question::Asker;
use crate::sandbox::{Containers, Sandbox};
use crate::session::{Entry, SessionError, SessionLog};
use crate::session_metadata::SessionMetadata;
use crate::session_name::Naming;
use crate::skills::{self, Skill};
use crate::tools::project::Project;
use crate::tools::{Approver, Land, SessionInfo, ToolBox};
use crate::workspace::{self, SessionSummary, SkillListing};
use crate::worktree;

/// The Session id that asks to resume the newest Session, archived or not.
pub const LATEST: &str = "latest";

/// The name of the directory that keeps the harness's files apart from others in the user's
/// shared directories.
const DIRECTORY_NAME: &str = "sstack";

/// A reason a Session could not be readied.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    /// The operating system gives no place for the user's data or settings, so the harness has
    /// nowhere to keep its files.
    #[error("cannot find the user data directory")]
    NoUserDirectory,
    /// [`LATEST`] was asked for, but the project has no Session yet.
    #[error("no session to resume")]
    NothingToResume,
    /// The project, its Sessions or its Workspace could not be read.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The user's settings are unreadable or name a Connection they do not define.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The Session's log could not be opened or repaired, as when it is open elsewhere.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The Connection's secret is in none of the places it is looked for.
    #[error(transparent)]
    MissingSecret(#[from] MissingSecret),
    /// The Connection's Provider could not be set up with its secret.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Returns where the harness keeps its files within `base`, failing if `base` is unknown.
fn app_dir(base: Option<PathBuf>) -> Result<PathBuf, SetupError> {
    let base = base.ok_or(SetupError::NoUserDirectory)?;
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

/// Which Session to work on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start<'a> {
    /// The Session with this id, or the newest one for [`LATEST`], archived or not.
    Resume(&'a str),
    /// A new Session in this place.
    New(Place),
}

/// The Connection a run was asked to use, as with `--connection`. It applies to the first
/// Session the run opens and to every Session it starts, which then keep it, but never moves a
/// stored Session opened later off the Connection that Session chose.
#[derive(Debug, Clone, Default)]
pub struct AskedConnection {
    /// The name of the Connection asked for. Missing if the run asked for none.
    name: Option<String>,
    /// Whether the run has opened a Session yet.
    opened: bool,
}

impl AskedConnection {
    /// Remembers that the run asked for the Connection called `name`, if any.
    #[must_use]
    pub fn new(name: Option<String>) -> Self {
        Self {
            name,
            opened: false,
        }
    }

    /// Returns the Connection to open the Session that `start` asks for on, for
    /// [`prepare`]. Missing leaves the Session on the one it chose.
    pub fn for_opening(&mut self, start: Start<'_>) -> Option<&str> {
        let first = !std::mem::replace(&mut self.opened, true);
        let applies = first || matches!(start, Start::New(_));
        self.name.as_deref().filter(|_| applies)
    }
}

/// A Session log that is open and ready for an agent.
struct OpenedLog {
    /// Where the Session is stored.
    path: PathBuf,
    /// The durable record of the Session.
    log: SessionLog,
    /// The Session so far.
    entries: Vec<Entry>,
    /// Whether the Session should get a worktree if it has none.
    wants_worktree: bool,
}

/// Opens the log of the Session that `start` asks for.
fn open_session(directory: &Path, start: Start<'_>) -> Result<OpenedLog, SetupError> {
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
fn resume_session(directory: &Path, wanted: &str) -> Result<OpenedLog, SetupError> {
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
fn resume_path(directory: &Path, wanted: &str) -> Result<PathBuf, SetupError> {
    if wanted == LATEST {
        let latest = workspace::latest_session(directory)?;
        latest.ok_or(SetupError::NothingToResume)
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
    /// How many Skills the system prompt lists by name alone, because their descriptions did not
    /// fit the listing's share of the context window. The person should hear of it when this is
    /// not zero, after the [`notices`](Self::notices), since the model is then less likely to
    /// load those Skills when they would help.
    pub descriptions_left_out: usize,
    /// Whether the person wants to type into a real nvim.
    pub vim: bool,
    /// Every Skill installed for the Session, which the person can run by name.
    pub skills: Vec<Skill>,
    /// The way to give the Session its Session Name from the first thing the person says in it.
    /// Missing if the Session already has a Session Name or a first prompt to be known by.
    pub naming: Option<Naming>,
    /// The Connections the Session can switch between, with the one it runs on now.
    pub connections: Connections,
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

/// Readies a harness for the project the person started in, with the [`Connection`] and model
/// they ask for.
///
/// # Errors
/// Fails if `directory` is in no readable project, if the user's settings are unusable, if the
/// Session cannot be found or is open elsewhere, or if the Connection cannot be set up.
#[builder]
pub fn prepare(
    /// Which Session to work on.
    start: Start<'_>,
    /// The directory the person started in, which picks the project and any Skills of a
    /// directory nested inside it.
    directory: &Path,
    /// The name of the Connection to run the Session on from now on, which the Session keeps.
    /// Missing means the one the Session last ran on, or else the default in settings.
    connection: Option<&str>,
    /// The identifier of the model to request. Missing means the Connection's model, or else the
    /// default in settings.
    model: Option<&str>,
    /// Makes, from the Session's id, whoever decides whether each shell command the model asks to
    /// run on the host may run.
    approver: impl FnOnce(&str) -> Arc<dyn Approver>,
    /// Makes, from the Session's id, whoever answers the model's Questions. With none, as when
    /// nobody is there to answer, the model cannot ask any.
    asker: impl FnOnce(&str) -> Option<Arc<dyn Asker>>,
    /// The runtime for the Session's Sandbox, shared with every other Session of the run.
    containers: Arc<Containers>,
) -> Result<Run, SetupError> {
    let config = load_settings()?;
    if let Some(asked) = connection {
        config.connection(Some(asked))?;
    }
    let main = main_checkout(directory)?;
    let session = open_session(&project_sessions(&main)?, start)?;
    let chosen = chosen_connection(&config, connection, &session.path)?;
    let connection = config.connection(Some(&chosen.name))?;
    let backend = Backend {
        connection,
        model: model.unwrap_or_else(|| config.model(connection)).to_owned(),
        main: &main,
        secrets: config
            .connections
            .values()
            .map(|c| c.secret.clone())
            .collect(),
    };
    let name_model = config.name_model(connection).to_owned();
    let id = workspace::session_id(&session.path);
    let place = workplace(&main, &id, session.wants_worktree);
    let installed = installed_skills(place.worktree.as_deref().unwrap_or(&main), directory);
    let listing = SkillListing::new(&offered_skills(&installed), connection.context_limit);
    let configured = agent_config()
        .backend(&backend)
        .id(&id)
        .place(&place)
        .approver(approver(&id))
        .maybe_asker(asker(&id))
        .containers(containers)
        .skills(&installed)
        .listing(&listing)
        .call()?;
    let naming = wants_name(&session).then(|| {
        let naming = Naming::new(configured.namer, name_model, session.path.clone());
        match place.worktree {
            Some(_) => naming.with_branch(main.clone()),
            None => naming,
        }
    });
    let connections = Connections::builder()
        .config(config.clone())
        .current(chosen.name)
        .log(session.path.clone())
        .main(main)
        .keyring(Arc::new(SystemKeyring))
        .build();
    Ok(Run {
        agent: Agent::new(configured.agent, session.log, session.entries)?,
        session: session.path,
        notices: chosen.notice.into_iter().chain(place.notice).collect(),
        descriptions_left_out: listing.without_descriptions,
        vim: config.vim,
        skills: installed,
        naming,
        connections,
    })
}

/// The Connection a Session runs on, as it is opened.
struct Chosen {
    /// The name of the Connection.
    name: String,
    /// What the person should know about the choice, when the Session could not have the
    /// Connection it last ran on.
    notice: Option<String>,
}

/// Decides which Connection the Session stored at `log` runs on: the one called `asked`, which
/// the Session keeps from now on, or else the one it last ran on, or else the default.
///
/// # Errors
/// Returns an error if the choice of `asked` cannot be recorded.
fn chosen_connection(
    config: &Config,
    asked: Option<&str>,
    log: &Path,
) -> Result<Chosen, SetupError> {
    if let Some(asked) = asked {
        connections::record(log, asked)?;
        return Ok(Chosen {
            name: asked.to_owned(),
            notice: None,
        });
    }
    let saved = SessionMetadata::read(log)?.connection;
    let default = config.default_connection.clone();
    Ok(match saved {
        Some(saved) if config.connections.contains_key(&saved) => Chosen {
            name: saved,
            notice: None,
        },
        Some(saved) => Chosen {
            notice: Some(format!(
                "this Session ran on the Connection {saved}, which settings no longer define, \
                 so it runs on {default}"
            )),
            name: default,
        },
        None => Chosen {
            name: default,
            notice: None,
        },
    })
}

/// Returns whether the opened Session still needs a Session Name: it has neither one of its own
/// nor a first prompt to be known by.
fn wants_name(session: &OpenedLog) -> bool {
    let spoken = (session.entries.iter()).any(|entry| matches!(entry, Entry::User { .. }));
    !spoken && !workspace::is_named(&session.path)
}

/// Returns the main checkout of the project that `directory` is in, with symlinks resolved.
///
/// # Errors
/// Fails if `directory` cannot be read.
pub fn main_checkout(directory: &Path) -> Result<PathBuf, SetupError> {
    Ok(Project::new(directory)?.root().to_owned())
}

/// Returns where the Sessions of the project that `directory` is in are stored.
///
/// # Errors
/// Fails if `directory` cannot be read or the user data directory is unknown.
pub fn sessions_directory(directory: &Path) -> Result<PathBuf, SetupError> {
    project_sessions(&main_checkout(directory)?)
}

/// Describes the stored Sessions of the project whose main checkout is `main` that are worth
/// listing, with the Sessions stored in `directory`: those with something in them, those the
/// person named, and blank ones with a worktree, which would otherwise be left behind unnoticed.
///
/// # Errors
/// Fails if `directory` exists but cannot be read.
pub fn listed_sessions(main: &Path, directory: &Path) -> io::Result<Vec<SessionSummary>> {
    let mut stored = workspace::list_sessions(directory)?;
    stored.retain(|summary| {
        let id = workspace::session_id(&summary.path);
        let named = summary.first_prompt.is_some() || summary.name.is_some();
        named || worktree::path(main, &id).exists()
    });
    Ok(stored)
}

/// Picks the Session to show first: the one `resume` names, or else a blank Session. One that an
/// earlier run left unused and unnamed among `stored` is reused, so that merely launching does
/// not pile up worktrees.
#[must_use]
pub fn starting_session<'a>(resume: Option<&'a str>, stored: &'a [SessionSummary]) -> Start<'a> {
    let unused = stored
        .iter()
        .find(|summary| summary.first_prompt.is_none() && summary.name.is_none())
        .and_then(|summary| summary.path.file_stem()?.to_str());
    resume
        .or(unused)
        .map_or(Start::New(Place::Worktree), Start::Resume)
}

/// Returns where the Sessions of the project at `root` are stored.
fn project_sessions(root: &Path) -> Result<PathBuf, SetupError> {
    Ok(workspace::sessions_dir(&app_dir(dirs::data_dir())?, root))
}

/// Returns the user's settings.
fn load_settings() -> Result<Config, SetupError> {
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
    /// The names of the secrets of every Connection in settings, which are kept from commands
    /// on the host, since the Session can switch to any of them.
    secrets: Vec<String>,
}

/// Finds the [`Skill`]s for a Session working in the project at `root`: the project's own,
/// including those of `directory`, where the person started, if it lies inside `root`, and the
/// person's.
#[must_use]
pub fn installed_skills(root: &Path, directory: &Path) -> Vec<Skill> {
    let home = dirs::home_dir();
    skills::discover(&skills::locations(root, directory, home.as_deref()))
}

/// Returns the ones among `installed` that the model may load on its own.
fn offered_skills(installed: &[Skill]) -> Vec<Skill> {
    installed
        .iter()
        .filter(|skill| skill.model_invocable)
        .cloned()
        .collect()
}

/// What a Session's models run on.
struct Configured {
    /// What the agent runs on.
    agent: AgentConfig,
    /// The service that runs the model that names the Session, on the same Connection as the
    /// agent's but asking for little effort.
    namer: Arc<dyn Provider>,
}

/// Decides what the agent for a Session, and the model that names it, run on.
#[builder]
fn agent_config(
    /// The model the run has chosen.
    backend: &Backend<'_>,
    /// The id of the Session.
    id: &str,
    /// Where the Session works.
    place: &Workplace,
    /// Who decides on the Session's shell commands that would run on the host.
    approver: Arc<dyn Approver>,
    /// Who answers the model's Questions in the Session, if anyone does.
    asker: Option<Arc<dyn Asker>>,
    /// The runtime for the Session's Sandbox.
    containers: Arc<Containers>,
    /// Every Skill installed for the Session. The model may read the files of all of them, since
    /// a person may run any of them, but is offered only those it may load on its own.
    skills: &[Skill],
    /// How the system prompt lists those Skills.
    listing: &SkillListing,
) -> Result<Configured, SetupError> {
    let connection = backend.connection;
    SystemKeyring::connect();
    let secret = resolve_secret(&connection.secret, &SystemKeyring, backend.main)?;
    let root = place.worktree.as_deref().unwrap_or(backend.main);
    let skill_directories = skills
        .iter()
        .filter_map(|s| s.path.parent().map(Path::to_owned));
    let project = Arc::new(Project::new(root)?.with_readable(skill_directories));
    let secret_env = backend.secrets.clone();
    let sandbox = Sandbox::new(containers, id, project.root());
    let tools = ToolBox::standard()
        .project(project.clone())
        .sandbox(Arc::new(sandbox))
        .approver(approver.clone())
        .secret_env(secret_env.clone())
        .call()
        .with_session_info(SessionInfo {
            session_id: id.to_owned(),
            model: backend.model.clone(),
            effort: Some(connection.effort),
        })
        .with_skills(offered_skills(skills))
        .with_asker(asker);
    let home = dirs::home_dir();
    let claude = skills::claude_directory(home.as_deref(), |name| std::env::var_os(name));
    let system = workspace::system_prompt(root, claude.as_deref());
    let system = workspace::skills_prompt(&system, listing);
    let (tools, system) = if place.worktree.is_some() {
        let land = Land {
            worktree: project,
            main: backend.main.to_owned(),
            approver,
            secret_env,
        };
        (tools.with_land(land), workspace::worktree_prompt(&system))
    } else {
        (tools, system)
    };
    let opened = connections::open(connection, secret)?;
    Ok(Configured {
        namer: opened.namer,
        agent: AgentConfig {
            provider: opened.provider,
            model: backend.model.clone(),
            system,
            tools,
            context_limit: connection.context_limit,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_asked_connection_applies_to_the_first_session_and_new_ones() {
        let mut asked = AskedConnection::new(Some("work".into()));
        assert_eq!(asked.for_opening(Start::Resume("1")), Some("work"));
        assert_eq!(asked.for_opening(Start::Resume("2")), None);
        assert_eq!(asked.for_opening(Start::New(Place::Worktree)), Some("work"));
        let mut none = AskedConnection::default();
        assert_eq!(none.for_opening(Start::New(Place::Worktree)), None);
    }

    #[test]
    fn only_a_session_with_nothing_to_be_known_by_wants_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut new = open_session(dir.path(), Start::New(Place::MainCheckout)).unwrap();
        assert!(wants_name(&new));
        new.entries.push(Entry::User { text: "hi".into() });
        assert!(!wants_name(&new));
        new.entries.clear();
        let id = workspace::session_id(&new.path);
        workspace::name_session(dir.path(), &id, Some("named")).unwrap();
        assert!(!wants_name(&new));
    }

    #[test]
    fn a_new_session_is_created_and_can_be_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let mut new = open_session(dir.path(), Start::New(Place::Worktree)).unwrap();
        assert_eq!(new.entries, Vec::new());
        assert!(new.wants_worktree);
        new.log.append(&Entry::TurnCancelled).unwrap();
        let path = new.path;
        drop(new.log);

        let id = workspace::session_id(&path);
        let by_id = open_session(dir.path(), Start::Resume(&id)).unwrap();
        assert!(open_session(dir.path(), Start::Resume(LATEST)).is_err());
        drop(by_id.log);
        let newest = open_session(dir.path(), Start::Resume(LATEST)).unwrap();

        assert_eq!(by_id.path, path);
        assert_eq!(newest.path, path);
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

    /// Describes the stored Session with id `id`, which the person started with `first_prompt`.
    fn summary(id: &str, first_prompt: Option<&str>) -> SessionSummary {
        SessionSummary {
            path: PathBuf::from(format!("/s/{id}.jsonl")),
            first_prompt: first_prompt.map(str::to_owned),
            name: None,
            modified: std::time::UNIX_EPOCH,
            parked: false,
        }
    }

    #[test]
    fn launching_reuses_an_unused_session_unless_told_otherwise() {
        let stored = [summary("1", Some("hello")), summary("2", None)];
        let spoken = &stored[..1];
        let fresh = Start::New(Place::Worktree);

        assert_eq!(starting_session(None, &stored), Start::Resume("2"));
        assert_eq!(starting_session(None, spoken), fresh);
        assert_eq!(starting_session(Some("1"), &stored), Start::Resume("1"));

        let mut named = summary("3", None);
        named.name = Some("for later".into());
        assert_eq!(starting_session(None, &[named]), fresh);
    }

    #[test]
    fn blank_sessions_are_listed_only_with_a_worktree() {
        let main = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        for id in ["1", "2", "3"] {
            SessionLog::create(&workspace::session_path(sessions.path(), id)).unwrap();
        }
        let mut spoken = SessionLog::open(&workspace::session_path(sessions.path(), "1"))
            .unwrap()
            .0;
        spoken.append(&Entry::User { text: "hi".into() }).unwrap();
        drop(spoken);
        std::fs::create_dir_all(worktree::path(main.path(), "2")).unwrap();

        let listed = listed_sessions(main.path(), sessions.path()).unwrap();

        let mut ids: Vec<_> = listed
            .iter()
            .map(|s| workspace::session_id(&s.path))
            .collect();
        ids.sort();
        assert_eq!(ids, ["1", "2"]);
    }

    #[test]
    fn the_app_directory_needs_a_base() {
        assert!(app_dir(None).is_err());
        let found = app_dir(Some(PathBuf::from("/base"))).unwrap();
        assert_eq!(found, Path::new("/base/sstack"));
    }
}
