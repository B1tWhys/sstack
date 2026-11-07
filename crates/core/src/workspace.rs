//! Where a project's Sessions live, and the instructions a Session starts with.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The instructions every Session starts with.
const BASE_PROMPT: &str = "You are sstack, a coding agent working in the user's project. \
Use the tools to inspect and change the project, and verify your changes before finishing. \
Be direct, and report results faithfully, including anything that failed.";
/// The file a project keeps its own instructions in.
const INSTRUCTIONS_FILE: &str = "AGENTS.md";
/// The file extension of Session logs.
const SESSION_EXTENSION: &str = "jsonl";

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
    let entries = match fs::read_dir(sessions_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let paths = entries.collect::<io::Result<Vec<_>>>()?;
    Ok(paths.into_iter().map(|entry| entry.path()).max())
}

/// Builds the system prompt from the base instructions and the project's own, if any.
#[must_use]
pub fn system_prompt(project: &Path) -> String {
    match fs::read_to_string(project.join(INSTRUCTIONS_FILE)) {
        Ok(instructions) => format!("{BASE_PROMPT}\n\n{instructions}"),
        Err(_) => BASE_PROMPT.to_owned(),
    }
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
    fn a_file_in_place_of_the_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(latest_session(&file).is_err());
    }

    #[test]
    fn project_instructions_extend_the_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(system_prompt(dir.path()), BASE_PROMPT);
        fs::write(dir.path().join(INSTRUCTIONS_FILE), "Use tabs.").unwrap();
        assert!(system_prompt(dir.path()).ends_with("\n\nUse tabs."));
    }
}
