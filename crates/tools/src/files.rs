use std::collections::HashMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::parse_input;
use super::project::Project;
use sstack::connection::ToolSpec;
use sstack::tools::{Tool, ToolError};

/// The most lines `read` returns unless asked for fewer or more.
const DEFAULT_LINE_LIMIT: usize = 2000;

/// Describes a filesystem failure, naming the path the model asked for.
fn io_error(path: &str, error: &std::io::Error) -> ToolError {
    ToolError::Failed(format!("{path}: {error}"))
}

/// What the model last saw of each file, so that `edit` and `write` only change a
/// file the model knows as it is now. Without it, an edit could land on a file that a command, a
/// formatter or the person has changed since, and line numbers the model remembers would point at
/// the wrong lines.
///
/// One Session's file tools share one, and it is kept only in memory, so after a Rebuild the
/// model has to read a file again before changing it.
#[derive(Default)]
pub(super) struct Seen {
    /// A hash of each file's contents when the model last read or changed it, by resolved path.
    hashes: Mutex<HashMap<PathBuf, u64>>,
}

impl Seen {
    /// Notes that the model now knows the file at `path` holds `contents`, whether it read
    /// them or wrote them. Reading part of a file counts, as the model can read the rest.
    fn record(&self, path: &Path, contents: &str) {
        self.hashes().insert(path.to_owned(), hash(contents));
    }

    /// Checks that `contents`, which the file at `path` holds now, are what the model last saw.
    ///
    /// # Errors
    /// Returns why not, if the model has not read the file or it has changed since.
    fn check(&self, path: &Path, contents: &str) -> Result<(), Unseen> {
        match self.hashes().get(path) {
            Some(&seen) if seen == hash(contents) => Ok(()),
            Some(_) => Err(Unseen::Changed),
            None => Err(Unseen::NeverRead),
        }
    }

    /// Gives access to the hashes, even after a panic elsewhere, since they stay consistent.
    fn hashes(&self) -> MutexGuard<'_, HashMap<PathBuf, u64>> {
        self.hashes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why the model may not change a file until it reads it again.
enum Unseen {
    /// The model has not read the file since the Running Harness started.
    NeverRead,
    /// The file has changed since the model last read or changed it.
    Changed,
}

impl Unseen {
    /// Tells the model why it cannot change the file it calls `shown`, and what to do instead.
    fn error(self, shown: &str) -> ToolError {
        ToolError::Failed(match self {
            Self::NeverRead => format!("{shown} has not been read yet; read it before changing it"),
            Self::Changed => format!(
                "{shown} has changed since you last read it; read it again before changing it"
            ),
        })
    }
}

/// Fingerprints a file's contents, for telling whether they have changed. Only comparable
/// within one Running Harness.
fn hash(contents: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);
    hasher.finish()
}

/// The tool for reading text files in the project.
pub(super) struct Read {
    /// The project files are read from.
    project: Arc<Project>,
    /// What the model has seen, told of every file read so that the file can later be changed.
    seen: Arc<Seen>,
}

impl Read {
    /// Creates the tool for `project`, sharing `seen` with the tools that change files.
    pub(super) fn new(project: Arc<Project>, seen: Arc<Seen>) -> Self {
        Self { project, seen }
    }
}

/// The input to the `read` tool.
#[derive(Deserialize)]
struct ReadInput {
    /// The file, relative to the project root.
    path: String,
    /// How many lines to skip.
    #[serde(default)]
    offset: usize,
    /// The most lines to return.
    limit: Option<usize>,
}

#[async_trait]
impl Tool for Read {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a text file in the project, or in the directory of a loaded Skill. \
                Lines are numbered from 1."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the project root, or an absolute path inside a loaded Skill's directory"},
                    "offset": {"type": "integer", "description": "Number of lines to skip"},
                    "limit": {"type": "integer", "description": "Maximum lines to return"}
                },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let input: ReadInput = parse_input(input)?;
        let path = self.project.resolve_for_reading(&input.path)?;
        let contents = fs::read_to_string(&path).map_err(|e| io_error(&input.path, &e))?;
        self.seen.record(&path, &contents);
        let numbered: Vec<String> = contents
            .lines()
            .enumerate()
            .skip(input.offset)
            .take(input.limit.unwrap_or(DEFAULT_LINE_LIMIT))
            .map(|(index, line)| format!("{}\t{line}", index + 1))
            .collect();
        Ok(numbered.join("\n"))
    }
}

/// The tool for creating files and replacing their whole contents.
pub(super) struct Write {
    /// The project files are written to.
    project: Arc<Project>,
    /// What the model has seen, which decides whether it knows an existing file well enough to
    /// replace it.
    seen: Arc<Seen>,
}

impl Write {
    /// Creates the tool for `project`, sharing `seen` with the other file tools.
    pub(super) fn new(project: Arc<Project>, seen: Arc<Seen>) -> Self {
        Self { project, seen }
    }

    /// Checks that the file at `path` may be replaced: either it does not exist yet, or the
    /// model has seen it as it is now.
    ///
    /// # Errors
    /// Returns an error, naming the file as `shown`, if the model has not seen the file as it
    /// is now or it cannot be read.
    fn check_replaceable(&self, path: &Path, shown: &str) -> Result<(), ToolError> {
        match fs::read_to_string(path) {
            Ok(contents) => self
                .seen
                .check(path, &contents)
                .map_err(|unseen| unseen.error(shown)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(shown, &error)),
        }
    }
}

/// The input to the `write` tool.
#[derive(Deserialize)]
struct WriteInput {
    /// The file, relative to the project root.
    path: String,
    /// The file's complete new contents.
    content: String,
}

#[async_trait]
impl Tool for Write {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Create a file in the project, or replace all of its contents. Replacing \
                an existing file fails unless you have read it since it last changed."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the project root"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let input: WriteInput = parse_input(input)?;
        let path = self.project.resolve(&input.path)?;
        self.check_replaceable(&path, &input.path)?;
        let write = || {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, &input.content)
        };
        write().map_err(|e| io_error(&input.path, &e))?;
        self.seen.record(&path, &input.content);
        Ok(format!("wrote {}", input.path))
    }
}

/// The tool for changing part of a file without rewriting it.
pub(super) struct Edit {
    /// The project files are edited in.
    project: Arc<Project>,
    /// What the model has seen, which decides whether it knows a file well enough to change it.
    seen: Arc<Seen>,
}

impl Edit {
    /// Creates the tool for `project`, sharing `seen` with the other file tools.
    pub(super) fn new(project: Arc<Project>, seen: Arc<Seen>) -> Self {
        Self { project, seen }
    }

    /// Applies the edit to the file at `path` and returns how many places it changed.
    ///
    /// # Errors
    /// Returns an error if the model has not seen the file as it is now, the text does not
    /// match as asked, or the file cannot be read or written.
    fn edit_file(&self, path: &Path, input: &EditInput) -> Result<usize, ToolError> {
        let contents = fs::read_to_string(path).map_err(|e| io_error(&input.path, &e))?;
        self.seen
            .check(path, &contents)
            .map_err(|unseen| unseen.error(&input.path))?;
        let (updated, count) = replace(&contents, input)?;
        fs::write(path, &updated).map_err(|e| io_error(&input.path, &e))?;
        self.seen.record(path, &updated);
        Ok(count)
    }
}

/// The input to the `edit` tool.
#[derive(Deserialize)]
struct EditInput {
    /// The file, relative to the project root.
    path: String,
    /// The text to replace, which must appear exactly once unless `replace_all` is set.
    old: String,
    /// The replacement text.
    new: String,
    /// Whether to replace every match instead of requiring exactly one. Off unless given.
    #[serde(default)]
    replace_all: bool,
}

/// Returns `contents` with the edit applied and how many places changed.
///
/// # Errors
/// Returns an error, telling the model how to fix the call, if `old` is empty, never matches,
/// or matches more than once without `replace_all`.
fn replace(contents: &str, input: &EditInput) -> Result<(String, usize), ToolError> {
    if input.old.is_empty() {
        return Err(ToolError::Failed(
            "`old` is empty; to replace a whole file, use write".to_owned(),
        ));
    }
    match (contents.matches(&input.old).count(), input.replace_all) {
        (0, _) => Err(ToolError::Failed(format!(
            "text not found in {}; read it again and copy the text exactly, leaving out the \
             line number and tab that read puts before each line",
            input.path
        ))),
        (count, true) | (count @ 1, false) => Ok((contents.replace(&input.old, &input.new), count)),
        (count, false) => Err(ToolError::Failed(format!(
            "text matches {count} times in {}; include more surrounding text to pick one, or \
             set replace_all to change every one",
            input.path
        ))),
    }
}

/// Reports a successful edit, with the number of places changed when there was more than one.
fn edited(path: &str, count: usize) -> String {
    if count == 1 {
        format!("edited {path}")
    } else {
        format!("edited {path}: {count} replacements")
    }
}

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace text in a project file. `old` must match exactly once unless \
                `replace_all` is set. Copy it from read's output without the line number and tab \
                before each line. Fails unless you have read the file since it last changed; \
                your own edits and writes count as reading it."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the project root"},
                    "old": {"type": "string"},
                    "new": {"type": "string"},
                    "replace_all": {"type": "boolean", "description": "Replace every match instead of exactly one, as when renaming"}
                },
                "required": ["path", "old", "new"]
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let input: EditInput = parse_input(input)?;
        let path = self.project.resolve(&input.path)?;
        let count = self.edit_file(&path, &input)?;
        Ok(edited(&input.path, count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::git_project;

    #[tokio::test]
    async fn read_numbers_lines_and_honours_the_window() {
        let (dir, project) = git_project();
        fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let read = FileTools::new(project).read;

        let all = read.run(json!({"path": "a.txt"})).await.unwrap();
        let window = read
            .run(json!({"path": "a.txt", "offset": 1, "limit": 1}))
            .await
            .unwrap();

        assert_eq!(all, "1\tone\n2\ttwo\n3\tthree");
        assert_eq!(window, "2\ttwo");
    }

    #[tokio::test]
    async fn read_reports_missing_and_hidden_files() {
        let (dir, project) = git_project();
        fs::write(dir.path().join(".env"), "KEY=secret").unwrap();
        let read = FileTools::new(project).read;

        assert!(read.run(json!({"path": "missing"})).await.is_err());
        let hidden = read.run(json!({"path": ".env"})).await.unwrap_err();
        assert_eq!(hidden.to_string(), "access denied: .env");
    }

    #[tokio::test]
    async fn write_creates_parent_directories_and_replaces_contents() {
        let (dir, project) = git_project();
        let write = FileTools::new(project).write;

        for content in ["first", "second"] {
            write
                .run(json!({"path": "new/dir/a.txt", "content": content}))
                .await
                .unwrap();
        }

        let written = fs::read_to_string(dir.path().join("new/dir/a.txt")).unwrap();
        assert_eq!(written, "second");
    }

    #[tokio::test]
    async fn write_refuses_ignored_paths() {
        let (_dir, project) = git_project();
        let write = FileTools::new(project).write;
        let input = json!({"path": "ignored/a.txt", "content": "x"});
        assert!(write.run(input).await.is_err());
    }

    #[tokio::test]
    async fn write_reports_filesystem_failures() {
        let (dir, project) = git_project();
        fs::write(dir.path().join("file"), "").unwrap();
        let write = FileTools::new(project).write;
        let input = json!({"path": "file/child.txt", "content": "x"});
        assert!(write.run(input).await.is_err());
    }

    /// The file tools of one Session, sharing what the model has seen.
    struct FileTools {
        /// The tool that reads files and records what the model saw.
        read: Read,
        /// The tool that replaces whole files.
        write: Write,
        /// The tool that changes parts of files.
        edit: Edit,
    }

    impl FileTools {
        /// Builds the file tools for `project`, as one Session gets them.
        fn new(project: Arc<Project>) -> Self {
            let seen = Arc::new(Seen::default());
            Self {
                read: Read::new(project.clone(), seen.clone()),
                write: Write::new(project.clone(), seen.clone()),
                edit: Edit::new(project, seen),
            }
        }

        /// Reads `path` through the tool, as the model would before changing it.
        async fn look_at(&self, path: &str) {
            self.read.run(json!({"path": path})).await.unwrap();
        }
    }

    #[tokio::test]
    async fn edit_replaces_only_a_unique_match() {
        let (dir, project) = git_project();
        let path = dir.path().join("a.txt");
        fs::write(&path, "alpha beta beta").unwrap();
        let tools = FileTools::new(project);
        tools.look_at("a.txt").await;

        let unique = json!({"path": "a.txt", "old": "alpha", "new": "gamma"});
        tools.edit.run(unique).await.unwrap();
        let ambiguous = json!({"path": "a.txt", "old": "beta", "new": "x"});
        let missing = json!({"path": "a.txt", "old": "delta", "new": "x"});
        let empty = json!({"path": "a.txt", "old": "", "new": "x"});
        let absent_file = json!({"path": "b.txt", "old": "a", "new": "x"});

        let ambiguous = tools.edit.run(ambiguous).await.unwrap_err().to_string();
        assert!(ambiguous.contains("2 times") && ambiguous.contains("replace_all"));
        let missing = tools.edit.run(missing).await.unwrap_err().to_string();
        assert!(missing.contains("not found") && missing.contains("line number"));
        assert!(tools.edit.run(empty).await.is_err());
        assert!(tools.edit.run(absent_file).await.is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "gamma beta beta");
    }

    #[tokio::test]
    async fn edit_can_replace_every_match() {
        let (dir, project) = git_project();
        let path = dir.path().join("a.txt");
        fs::write(&path, "beta alpha beta").unwrap();
        let tools = FileTools::new(project);
        tools.look_at("a.txt").await;

        let all = json!({"path": "a.txt", "old": "beta", "new": "x", "replace_all": true});
        let missing = json!({"path": "a.txt", "old": "delta", "new": "x", "replace_all": true});

        let report = tools.edit.run(all).await.unwrap();
        assert!(report.contains("2 replacements"), "{report}");
        assert!(tools.edit.run(missing).await.is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "x alpha x");
    }

    #[tokio::test]
    async fn edit_changes_only_a_file_as_the_model_last_saw_it() {
        let (dir, project) = git_project();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one two three").unwrap();
        let tools = FileTools::new(project);
        let change = |old: &str, new: &str| json!({"path": "a.txt", "old": old, "new": new});

        let unread = tools
            .edit
            .run(change("one", "1"))
            .await
            .unwrap_err()
            .to_string();
        assert!(unread.contains("not been read"), "{unread}");

        tools.look_at("a.txt").await;
        tools.edit.run(change("one", "1")).await.unwrap();
        tools.edit.run(change("two", "2")).await.unwrap();

        fs::write(&path, "1 2 three four").unwrap();
        let stale = tools
            .edit
            .run(change("three", "3"))
            .await
            .unwrap_err()
            .to_string();
        assert!(stale.contains("changed since"), "{stale}");

        tools.look_at("a.txt").await;
        tools.edit.run(change("three", "3")).await.unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "1 2 3 four");
    }

    #[tokio::test]
    async fn write_replaces_only_a_file_as_the_model_last_saw_it() {
        let (dir, project) = git_project();
        let path = dir.path().join("a.txt");
        fs::write(&path, "old").unwrap();
        let tools = FileTools::new(project);
        let replace = json!({"path": "a.txt", "content": "new"});

        let unread = tools
            .write
            .run(replace.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(unread.contains("not been read"), "{unread}");

        tools.look_at("a.txt").await;
        fs::write(&path, "changed").unwrap();
        let stale = tools
            .write
            .run(replace.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(stale.contains("changed since"), "{stale}");

        tools.look_at("a.txt").await;
        tools.write.run(replace).await.unwrap();
        let after = json!({"path": "a.txt", "old": "new", "new": "newer"});
        tools.edit.run(after).await.unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "newer");
    }
}
