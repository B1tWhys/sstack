//! Where a project's Sessions live, and the instructions a Session starts with.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::session;
use crate::session_metadata::{self, SessionMetadata};
use crate::skills::Skill;

/// The instructions every Session starts with.
const BASE_PROMPT: &str = "You are sstack, a coding agent working in the user's project. \
Use the tools to inspect and change the project, and verify your changes before finishing. \
Be direct, and report results faithfully, including anything that failed.";
/// The file a project keeps its own instructions in.
const INSTRUCTIONS_FILE: &str = "AGENTS.md";
/// The file, in Claude Code's directory, a person keeps instructions for every project in.
const USER_INSTRUCTIONS_FILE: &str = "CLAUDE.md";
/// The file extension of Session logs.
const SESSION_EXTENSION: &str = "jsonl";
/// The directory, among a project's Sessions, of the ones the person has put away.
const ARCHIVE_DIR: &str = "archived";
/// The model's instructions for working in a Session that has its own worktree.
const WORKTREE_PROMPT: &str = "This Session has its own git worktree on its own branch. Other \
Sessions work in parallel in their own worktrees. Commit here, then call the `land` tool to move \
each working change onto the main branch. Do not change the main branch any other way. Before \
each `land` call, give the person a summary to approve it against: what changed, any side effects \
or limits, and how you verified it.";
/// The model's instructions on what the listed Skills are and when to use them.
const SKILLS_PROMPT: &str = "These Skills hold instructions for particular kinds of work. When \
one fits what you are about to do, call the `skill` tool with its name first and follow what it \
says.";
/// How many tokens of the context window pay for each character of the Skill listing. One
/// percent of the window goes to the listing, at a rough four characters per token.
const CONTEXT_TOKENS_PER_LISTING_CHARACTER: u64 = 25;

/// The directory holding Sessions for `project`, kept outside it so they are never committed.
#[must_use]
pub fn sessions_dir(data_dir: &Path, project: &Path) -> PathBuf {
    let key = project.to_string_lossy().replace(['/', '\\'], "-");
    data_dir.join("sessions").join(key)
}

/// Returns a path for a new Session. Session ids sort by age.
#[must_use]
pub fn new_session_path(sessions_dir: &Path) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    session_path(sessions_dir, &format!("{millis:016}"))
}

/// Returns the path of the Session with the given id.
#[must_use]
pub fn session_path(sessions_dir: &Path, id: &str) -> PathBuf {
    sessions_dir.join(id).with_extension(SESSION_EXTENSION)
}

/// Returns the id of the Session stored at `path`.
#[must_use]
pub fn session_id(path: &Path) -> String {
    path.file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
}

/// Finds the newest Session in `sessions_dir`, if there is one.
///
/// # Errors
/// Returns an error if the directory exists but cannot be read.
pub fn latest_session(sessions_dir: &Path) -> io::Result<Option<PathBuf>> {
    Ok(session_paths(sessions_dir)?.into_iter().max())
}

/// A stored Session, described well enough to choose it from a list without opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Where the Session is stored.
    pub path: PathBuf,
    /// The first thing the person said in it. Missing if they have not said anything yet.
    pub first_prompt: Option<String>,
    /// The name the person gave it, which is its Session Name in place of its first prompt.
    /// Missing if they have not named it.
    pub name: Option<String>,
    /// The last time anything was recorded in it. Naming it does not count.
    pub modified: SystemTime,
    /// Whether it stopped between Steps and should carry on.
    pub parked: bool,
}

/// Describes every Session in `sessions_dir`, except archived ones.
///
/// # Errors
/// Returns an error if the directory exists but cannot be read.
pub fn list_sessions(sessions_dir: &Path) -> io::Result<Vec<SessionSummary>> {
    let paths = session_paths(sessions_dir)?;
    Ok(paths.into_iter().filter_map(summarize).collect())
}

/// Describes the Session at `path`, or returns nothing if it cannot be read.
fn summarize(path: PathBuf) -> Option<SessionSummary> {
    let first_prompt = session::first_prompt(&path).ok()?;
    let modified = fs::metadata(&path).and_then(|file| file.modified()).ok()?;
    let parked = session::is_parked(&path).ok()?;
    let name = SessionMetadata::read(&path)
        .ok()
        .and_then(|metadata| metadata.name);
    Some(SessionSummary {
        path,
        first_prompt,
        name,
        modified,
        parked,
    })
}

/// Makes `name` the Session Name of the Session with id `id`. With `None`, takes the name away, so
/// that the Session goes back to being known by what the person said first.
///
/// # Errors
/// Returns an error if the name cannot be stored or removed.
pub fn name_session(sessions_dir: &Path, id: &str, name: Option<&str>) -> io::Result<()> {
    SessionMetadata::update(&session_path(sessions_dir, id), |metadata| {
        metadata.name = name.map(ToOwned::to_owned);
    })
}

/// Makes `name` the Session Name of the Session stored at `log`, unless it already has one,
/// returning whether it did. A name the person gives while `name` is being made is never
/// overwritten.
///
/// # Errors
/// Returns an error if the name cannot be stored.
pub fn name_unnamed_session(log: &Path, name: &str) -> io::Result<bool> {
    SessionMetadata::update(log, |metadata| {
        let unnamed = metadata.name.is_none();
        if unnamed {
            metadata.name = Some(name.to_owned());
        }
        unnamed
    })
}

/// Returns whether the Session stored at `log` has a Session Name of its own, rather than being
/// known by its first prompt. A Session whose metadata cannot be read counts as named, since no
/// name could be stored for it anyway.
#[must_use]
pub fn is_named(log: &Path) -> bool {
    !SessionMetadata::read(log).is_ok_and(|metadata| metadata.name.is_none())
}

/// Moves the Session logged at `from` to `to`, along with its Session Metadata. The log moves
/// last, so that a failure partway at worst leaves the Session where it was, without its
/// metadata.
fn move_session(from: &Path, to: &Path) -> io::Result<()> {
    session_metadata::relocate(from, to)?;
    fs::rename(from, to)
}

/// Returns the path of every Session log in `sessions_dir`, which need not exist.
fn session_paths(sessions_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(sessions_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let paths = entries.collect::<io::Result<Vec<_>>>()?;
    let is_log = |path: &PathBuf| path.extension().is_some_and(|ext| ext == SESSION_EXTENSION);
    let paths = paths.into_iter().map(|entry| entry.path());
    Ok(paths.filter(is_log).collect())
}

/// Returns where the Session with id `id` is stored once archived.
fn archived_path(sessions_dir: &Path, id: &str) -> PathBuf {
    session_path(&sessions_dir.join(ARCHIVE_DIR), id)
}

/// Puts the Session with id `id` away, so that it is no longer listed but can still be restored.
///
/// # Errors
/// Returns an error if the Session cannot be moved.
pub fn archive_session(sessions_dir: &Path, id: &str) -> io::Result<()> {
    let archived = archived_path(sessions_dir, id);
    if let Some(parent) = archived.parent() {
        fs::create_dir_all(parent)?;
    }
    move_session(&session_path(sessions_dir, id), &archived)
}

/// Brings the Session with id `id` back from the archive, returning whether it was there.
///
/// # Errors
/// Returns an error if the Session is archived but cannot be moved back.
pub fn restore_session(sessions_dir: &Path, id: &str) -> io::Result<bool> {
    let archived = archived_path(sessions_dir, id);
    if !archived.exists() {
        return Ok(false);
    }
    move_session(&archived, &session_path(sessions_dir, id))?;
    Ok(true)
}

/// Builds the system prompt from the base instructions, then the person's own and the project's
/// own, if there are any. The project's come last, so that they can overrule the person's.
///
/// `claude_directory` is where Claude Code keeps the person's files, if known.
#[must_use]
pub fn system_prompt(project: &Path, claude_directory: Option<&Path>) -> String {
    let user = claude_directory.map(|directory| directory.join(USER_INSTRUCTIONS_FILE));
    let files = user.into_iter().chain([project.join(INSTRUCTIONS_FILE)]);
    let mut prompt = BASE_PROMPT.to_owned();
    for instructions in files.filter_map(|file| fs::read_to_string(file).ok()) {
        prompt.push_str("\n\n");
        prompt.push_str(&instructions);
    }
    prompt
}

/// Extends `prompt` for a Session that has its own worktree.
#[must_use]
pub fn worktree_prompt(prompt: &str) -> String {
    format!("{prompt}\n\n{WORKTREE_PROMPT}")
}

/// The list of Skills the model is told about, cut to fit its share of the context window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillListing {
    /// One line per Skill, in the order given.
    lines: Vec<String>,
    /// How many Skills are listed by name alone, because their descriptions did not fit.
    pub without_descriptions: usize,
}

impl SkillListing {
    /// Lists `skills` within about one percent of a context window of `context_limit` tokens.
    ///
    /// Every Skill keeps its name. When the descriptions do not all fit, they are left out from
    /// the end of the list first, which is where the person's own Skills come.
    #[must_use]
    pub fn new(skills: &[Skill], context_limit: u64) -> Self {
        let budget = context_limit / CONTEXT_TOKENS_PER_LISTING_CHARACTER;
        let budget = usize::try_from(budget).unwrap_or(usize::MAX);
        let mut lines: Vec<String> = skills
            .iter()
            .map(|skill| format!("- {}: {}", skill.name, skill.description))
            .collect();
        let mut size: usize = lines.iter().map(|line| line.len() + 1).sum();
        let mut without_descriptions = 0;
        for (line, skill) in lines.iter_mut().zip(skills).rev() {
            if size <= budget {
                break;
            }
            let bare = format!("- {}", skill.name);
            size -= line.len() - bare.len();
            *line = bare;
            without_descriptions += 1;
        }
        Self {
            lines,
            without_descriptions,
        }
    }
}

/// Extends `prompt` so the model knows which Skills it can use, as `listing` lists them. With
/// none, leaves it as it is.
#[must_use]
pub fn skills_prompt(prompt: &str, listing: &SkillListing) -> String {
    if listing.lines.is_empty() {
        return prompt.to_owned();
    }
    format!(
        "{prompt}\n\n{SKILLS_PROMPT}\n\n{}",
        listing.lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::Scope;

    #[test]
    fn projects_get_separate_session_directories() {
        let data = Path::new("/data");
        let first = sessions_dir(data, Path::new("/code/a"));
        let second = sessions_dir(data, Path::new("/code/b"));
        assert_eq!(first, Path::new("/data/sessions/-code-a"));
        assert_ne!(first, second);
    }

    #[test]
    fn session_ids_round_trip_through_paths() {
        let dir = Path::new("/sessions");
        let path = new_session_path(dir);
        assert_eq!(session_path(dir, &session_id(&path)), path);
    }

    #[test]
    fn latest_session_is_the_highest_id() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(latest_session(&dir.path().join("none")).unwrap(), None);
        assert_eq!(latest_session(dir.path()).unwrap(), None);
        for id in ["0001", "0003", "0002"] {
            fs::write(session_path(dir.path(), id), "").unwrap();
        }
        let latest = latest_session(dir.path()).unwrap();
        assert_eq!(latest, Some(session_path(dir.path(), "0003")));
    }

    #[test]
    fn sessions_are_listed_with_what_the_person_said_first() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(list_sessions(&dir.path().join("none")).unwrap(), vec![]);
        let empty = session_path(dir.path(), "empty");
        fs::write(&empty, "").unwrap();
        let spoken = session_path(dir.path(), "spoken");
        let entries = "{\"type\":\"user\",\"text\":\"hello\"}\n{\"type\":\"turn_parked\"}\n";
        fs::write(&spoken, entries).unwrap();

        let mut listed = list_sessions(dir.path()).unwrap();
        listed.sort_by_key(|summary| summary.path.clone());

        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].path, empty);
        assert_eq!(listed[0].first_prompt, None);
        assert_eq!(listed[1].first_prompt.as_deref(), Some("hello"));
        assert!(listed[1].parked);
        assert!(!listed[0].parked);
        let modified = fs::metadata(&spoken).unwrap().modified().unwrap();
        assert_eq!(listed[1].modified, modified);
    }

    #[test]
    fn an_archived_session_stops_being_listed_until_it_is_restored() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(session_path(dir.path(), "1"), "").unwrap();

        archive_session(dir.path(), "1").unwrap();

        assert_eq!(list_sessions(dir.path()).unwrap(), vec![]);
        assert_eq!(latest_session(dir.path()).unwrap(), None);
        assert!(restore_session(dir.path(), "1").unwrap());
        assert_eq!(list_sessions(dir.path()).unwrap().len(), 1);
        assert!(!restore_session(dir.path(), "1").unwrap());
        assert!(archive_session(&dir.path().join("none"), "1").is_err());
    }

    #[test]
    fn a_session_is_listed_by_the_name_it_was_given_until_that_is_taken_away() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(session_path(dir.path(), "1"), "").unwrap();
        let listed_name = || list_sessions(dir.path()).unwrap()[0].name.clone();
        assert_eq!(listed_name(), None);

        name_session(dir.path(), "1", Some("first")).unwrap();
        name_session(dir.path(), "1", Some("fix the sidebar")).unwrap();
        assert_eq!(listed_name().as_deref(), Some("fix the sidebar"));

        name_session(dir.path(), "1", None).unwrap();
        assert_eq!(listed_name(), None);
        name_session(dir.path(), "1", None).unwrap();
    }

    #[test]
    fn a_session_keeps_its_name_through_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(session_path(dir.path(), "1"), "").unwrap();
        name_session(dir.path(), "1", Some("kept")).unwrap();

        archive_session(dir.path(), "1").unwrap();
        assert_eq!(list_sessions(dir.path()).unwrap(), vec![]);
        assert!(restore_session(dir.path(), "1").unwrap());

        let listed = list_sessions(dir.path()).unwrap();
        assert_eq!(listed[0].name.as_deref(), Some("kept"));
    }

    #[test]
    fn a_name_that_cannot_be_read_is_no_name_and_cannot_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(session_path(dir.path(), "1"), "").unwrap();
        let metadata = session_path(dir.path(), "1").with_extension("json");
        fs::create_dir(metadata).unwrap();
        assert_eq!(list_sessions(dir.path()).unwrap()[0].name, None);
        assert!(name_session(dir.path(), "1", None).is_err());
        assert!(name_session(dir.path(), "1", Some("x")).is_err());
    }

    #[test]
    fn a_file_in_place_of_the_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(latest_session(&file).is_err());
        assert!(list_sessions(&file).is_err());
    }

    #[test]
    fn worktree_sessions_are_told_how_to_land() {
        let prompt = worktree_prompt("base");
        assert!(prompt.starts_with("base\n\n"));
        assert!(prompt.contains("`land`"));
    }

    #[test]
    fn project_instructions_extend_the_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(system_prompt(dir.path(), None), BASE_PROMPT);
        fs::write(dir.path().join(INSTRUCTIONS_FILE), "Use tabs.").unwrap();
        assert!(system_prompt(dir.path(), None).ends_with("\n\nUse tabs."));
    }

    #[test]
    fn user_instructions_come_before_the_projects() {
        let dir = tempfile::tempdir().unwrap();
        let (project, claude) = (dir.path().join("project"), dir.path().join("claude"));
        fs::create_dir(&project).unwrap();
        fs::create_dir(&claude).unwrap();
        assert_eq!(system_prompt(&project, Some(&claude)), BASE_PROMPT);

        fs::write(claude.join(USER_INSTRUCTIONS_FILE), "No emoji.").unwrap();
        fs::write(project.join(INSTRUCTIONS_FILE), "Use tabs.").unwrap();

        let prompt = system_prompt(&project, Some(&claude));
        assert_eq!(prompt, format!("{BASE_PROMPT}\n\nNo emoji.\n\nUse tabs."));
    }

    #[test]
    fn skills_are_listed_for_the_model() {
        let skill = Skill {
            name: "review".into(),
            description: "Use when reviewing.".into(),
            path: "SKILL.md".into(),
            scope: Scope::Project,
            model_invocable: true,
        };
        let prompt = skills_prompt("base", &SkillListing::new(&[skill], 1_000_000));
        assert!(prompt.starts_with("base\n\n"));
        assert!(prompt.contains("`skill` tool"));
        assert!(prompt.ends_with("\n\n- review: Use when reviewing."));
    }

    #[test]
    fn no_skills_leave_the_prompt_alone() {
        assert_eq!(skills_prompt("base", &SkillListing::new(&[], 0)), "base");
    }

    /// Returns a Skill called `name` described by `description`.
    fn described(name: &str, description: &str) -> Skill {
        Skill {
            name: name.into(),
            description: description.into(),
            path: "SKILL.md".into(),
            scope: Scope::Project,
            model_invocable: true,
        }
    }

    #[test]
    fn descriptions_that_do_not_fit_are_left_out_from_the_end() {
        let skills = [
            described("a", "first"),
            described("b", "second"),
            described("c", "third"),
        ];
        let full: usize = ["- a: first", "- b: second", "- c: third"]
            .iter()
            .map(|line| line.len() + 1)
            .sum();
        let tokens_for = |characters: usize| {
            u64::try_from(characters).unwrap() * CONTEXT_TOKENS_PER_LISTING_CHARACTER
        };

        let roomy = SkillListing::new(&skills, tokens_for(full));
        let tight = SkillListing::new(&skills, tokens_for(full - 1));
        let tiny = SkillListing::new(&skills, 0);

        assert_eq!(roomy.without_descriptions, 0);
        assert_eq!(tight.lines, ["- a: first", "- b: second", "- c"]);
        assert_eq!(tight.without_descriptions, 1);
        assert_eq!(tiny.lines, ["- a", "- b", "- c"]);
        assert_eq!(tiny.without_descriptions, 3);
    }
}
