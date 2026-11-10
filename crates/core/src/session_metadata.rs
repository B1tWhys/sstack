//! Session Metadata: what is kept about a Session beside its log, because it is not part of the
//! conversation the model sees.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The file extension of the Session Metadata file beside a Session log.
const EXTENSION: &str = "json";

/// Facts about a Session that the person, not the model, decides, kept in one file beside its
/// log so that adding another needs no new file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    /// The Session Name, whether the person gave it or a model summed up their first input.
    /// Missing until either happens, while the Session is known by its first prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Facts written by a newer Harness Source, kept so that an older Running Harness, such as
    /// one relaunched from an earlier commit, does not lose them when it changes the file.
    #[serde(flatten)]
    unknown: Map<String, Value>,
}

impl SessionMetadata {
    /// Returns the Session Metadata of the Session stored at `log`. A Session with none yet has
    /// the default, empty one.
    ///
    /// # Errors
    /// Returns an error if the metadata exists but cannot be read or understood.
    pub fn read(log: &Path) -> io::Result<Self> {
        match File::open(path(log)) {
            Ok(mut file) => {
                file.lock_shared()?;
                stored(&mut file)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Changes the Session Metadata of the Session stored at `log` with `change`, returning what
    /// `change` returns. Changes made at once, in this process or another, apply one after the
    /// other, so none is lost.
    ///
    /// # Errors
    /// Returns an error if the metadata cannot be read, understood or written. Nothing is changed
    /// then.
    pub fn update<T>(log: &Path, change: impl FnOnce(&mut Self) -> T) -> io::Result<T> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path(log))?;
        file.lock()?;
        let mut metadata = stored(&mut file)?;
        let changed = change(&mut metadata);
        let json = serde_json::to_vec_pretty(&metadata)?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&json)?;
        Ok(changed)
    }
}

/// Returns where the Session Metadata of the Session stored at `log` is kept.
fn path(log: &Path) -> PathBuf {
    log.with_extension(EXTENSION)
}

/// Returns the metadata in `file`. A file just created, and so empty, holds the default.
fn stored(file: &mut File) -> io::Result<SessionMetadata> {
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    if contents.is_empty() {
        return Ok(SessionMetadata::default());
    }
    Ok(serde_json::from_str(&contents)?)
}

/// Moves the Session Metadata of the Session stored at `from`, if it has any, to go with the log
/// at `to`.
///
/// # Errors
/// Returns an error if the metadata exists but cannot be moved.
pub fn relocate(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(path(from), path(to)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the log path of a Session in `dir`.
    fn log(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("1.jsonl")
    }

    #[test]
    fn a_session_without_metadata_has_the_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            SessionMetadata::read(&log(&dir)).unwrap(),
            SessionMetadata::default()
        );
    }

    #[test]
    fn changes_are_kept_and_returned() {
        let dir = tempfile::tempdir().unwrap();
        let log = log(&dir);
        let returned = SessionMetadata::update(&log, |metadata| {
            metadata.name = Some("named".into());
            7
        });
        assert_eq!(returned.unwrap(), 7);
        let read = SessionMetadata::read(&log).unwrap();
        assert_eq!(read.name.as_deref(), Some("named"));

        SessionMetadata::update(&log, |metadata| metadata.name = None).unwrap();

        assert_eq!(SessionMetadata::read(&log).unwrap().name, None);
        let written = fs::read_to_string(path(&log)).unwrap();
        assert!(!written.contains("name"));
    }

    #[test]
    fn facts_from_a_newer_harness_survive_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let log = log(&dir);
        fs::write(path(&log), r#"{"name":"n","pinned":true}"#).unwrap();

        SessionMetadata::update(&log, |metadata| metadata.name = None).unwrap();

        let written: Value =
            serde_json::from_str(&fs::read_to_string(path(&log)).unwrap()).unwrap();
        assert_eq!(written, serde_json::json!({"pinned": true}));
    }

    #[test]
    fn unreadable_metadata_is_an_error_and_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let log = log(&dir);
        fs::write(path(&log), "not json").unwrap();
        assert!(SessionMetadata::read(&log).is_err());
        assert!(SessionMetadata::update(&log, |_| ()).is_err());
        assert_eq!(fs::read_to_string(path(&log)).unwrap(), "not json");

        fs::remove_file(path(&log)).unwrap();
        fs::create_dir(path(&log)).unwrap();
        assert!(SessionMetadata::read(&log).is_err());
        assert!(SessionMetadata::update(&log, |_| ()).is_err());
    }

    #[test]
    fn metadata_moves_with_its_session_if_it_has_any() {
        let dir = tempfile::tempdir().unwrap();
        let from = log(&dir);
        let to = dir.path().join("2.jsonl");
        relocate(&from, &to).unwrap();
        fs::write(path(&from), "{}").unwrap();

        relocate(&from, &to).unwrap();

        assert!(path(&to).exists() && !path(&from).exists());
    }

    #[test]
    fn metadata_that_cannot_be_moved_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (log(&dir), dir.path().join("2.jsonl"));
        fs::write(path(&from), "{}").unwrap();
        fs::create_dir(path(&to)).unwrap();
        fs::write(path(&to).join("in the way"), "").unwrap();

        assert!(relocate(&from, &to).is_err());
    }
}
