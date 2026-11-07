//! Where a project's Sessions live, and the instructions a Session starts with.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::session;

/// The instructions every Session starts with.
const BASE_PROMPT: &str = "You are sstack, a coding agent working in the user's project. \
Use the tools to inspect and change the project, and verify your changes before finishing. \
Be direct, and report results faithfully, including anything that failed.";
/// The file a project keeps its own instructions in.
const INSTRUCTIONS_FILE: &str = "AGENTS.md";
/// The file extension of Session logs.
const SESSION_EXTENSION: &str = "jsonl";
/// The directory, among a project's Sessions, of the ones the person has put away.
const ARCHIVE_DIR: &str = "archived";
/// Tells the model how to work in a Session that has its own worktree.
const WORKTREE_PROMPT: &str = "This Session has its own git worktree on its own branch. Other \
Sessions work in parallel in their own worktrees. Commit here, then call the `land` tool to move \
each working change onto the main branch. Do not change the main branch any other way.";

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
    /// The last time anything was recorded in it.
    pub modified: SystemTime,
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
    Some(SessionSummary {
        path,
        first_prompt,
        modified,
    })
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
    fs::rename(session_path(sessions_dir, id), archived)
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
    fs::rename(archived, session_path(sessions_dir, id))?;
    Ok(true)
}

/// Builds the system prompt from the base instructions and the project's own, if any.
#[must_use]
pub fn system_prompt(project: &Path) -> String {
    match fs::read_to_string(project.join(INSTRUCTIONS_FILE)) {
        Ok(instructions) => format!("{BASE_PROMPT}\n\n{instructions}"),
        Err(_) => BASE_PROMPT.to_owned(),
    }
}

/// Extends `prompt` for a Session that has its own worktree.
#[must_use]
pub fn worktree_prompt(prompt: &str) -> String {
    format!("{prompt}\n\n{WORKTREE_PROMPT}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        fs::write(&spoken, "{\"type\":\"user\",\"text\":\"hello\"}\n").unwrap();

        let mut listed = list_sessions(dir.path()).unwrap();
        listed.sort_by_key(|summary| summary.path.clone());

        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].path, empty);
        assert_eq!(listed[0].first_prompt, None);
        assert_eq!(listed[1].first_prompt.as_deref(), Some("hello"));
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
        assert_eq!(system_prompt(dir.path()), BASE_PROMPT);
        fs::write(dir.path().join(INSTRUCTIONS_FILE), "Use tabs.").unwrap();
        assert!(system_prompt(dir.path()).ends_with("\n\nUse tabs."));
    }
}
