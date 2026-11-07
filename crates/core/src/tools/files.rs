use std::fs;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::project::Project;
use super::{Tool, ToolError, parse_input};
use crate::provider::ToolSpec;

/// The most lines `read` returns unless asked for fewer or more.
const DEFAULT_LINE_LIMIT: usize = 2000;

/// Describes a filesystem failure, naming the path the model asked for.
fn io_error(path: &str, error: &std::io::Error) -> ToolError {
    ToolError(format!("{path}: {error}"))
}

/// Lets the model read text files in the project.
pub(super) struct Read {
    /// The project files are read from.
    project: Arc<Project>,
}

impl Read {
    /// Creates the tool for `project`.
    pub(super) fn new(project: Arc<Project>) -> Self {
        Self { project }
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
        let contents = fs::read_to_string(path).map_err(|e| io_error(&input.path, &e))?;
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

/// Lets the model create files and replace their whole contents.
pub(super) struct Write {
    /// The project files are written to.
    project: Arc<Project>,
}

impl Write {
    /// Creates the tool for `project`.
    pub(super) fn new(project: Arc<Project>) -> Self {
        Self { project }
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
            description: "Create a file in the project, or replace all of its contents.".into(),
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
        let write = || {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, &input.content)
        };
        write().map_err(|e| io_error(&input.path, &e))?;
        Ok(format!("wrote {}", input.path))
    }
}

/// Lets the model change part of a file without rewriting it.
pub(super) struct Edit {
    /// The project files are edited in.
    project: Arc<Project>,
}

impl Edit {
    /// Creates the tool for `project`.
    pub(super) fn new(project: Arc<Project>) -> Self {
        Self { project }
    }
}

/// The input to the `edit` tool.
#[derive(Deserialize)]
struct EditInput {
    /// The file, relative to the project root.
    path: String,
    /// The text to replace, which must appear exactly once.
    old: String,
    /// The replacement text.
    new: String,
}

/// Returns `contents` with the edit applied, or an error unless it matches exactly once.
fn replace_unique(contents: &str, input: &EditInput) -> Result<String, ToolError> {
    match contents.matches(&input.old).count() {
        1 => Ok(contents.replacen(&input.old, &input.new, 1)),
        0 => Err(ToolError(format!("text not found in {}", input.path))),
        count => Err(ToolError(format!(
            "text matches {count} times in {}; include more context",
            input.path
        ))),
    }
}

/// Applies the edit to the file at `path`.
fn edit_file(path: &Path, input: &EditInput) -> Result<(), ToolError> {
    let contents = fs::read_to_string(path).map_err(|e| io_error(&input.path, &e))?;
    let updated = replace_unique(&contents, input)?;
    fs::write(path, updated).map_err(|e| io_error(&input.path, &e))
}

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace text in a project file. `old` must match exactly once.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the project root"},
                    "old": {"type": "string"},
                    "new": {"type": "string"}
                },
                "required": ["path", "old", "new"]
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let input: EditInput = parse_input(input)?;
        let path = self.project.resolve(&input.path)?;
        edit_file(&path, &input)?;
        Ok(format!("edited {}", input.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::git_project;

    #[tokio::test]
    async fn read_numbers_lines_and_honours_the_window() {
        let (dir, project) = git_project();
        fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let read = Read::new(project);

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
        let read = Read::new(project);

        assert!(read.run(json!({"path": "missing"})).await.is_err());
        let hidden = read.run(json!({"path": ".env"})).await.unwrap_err();
        assert_eq!(hidden.0, "access denied: .env");
    }

    #[tokio::test]
    async fn write_creates_parent_directories_and_replaces_contents() {
        let (dir, project) = git_project();
        let write = Write::new(project);

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
        let write = Write::new(project);
        let input = json!({"path": "ignored/a.txt", "content": "x"});
        assert!(write.run(input).await.is_err());
    }

    #[tokio::test]
    async fn write_reports_filesystem_failures() {
        let (dir, project) = git_project();
        fs::write(dir.path().join("file"), "").unwrap();
        let write = Write::new(project);
        let input = json!({"path": "file/child.txt", "content": "x"});
        assert!(write.run(input).await.is_err());
    }

    #[tokio::test]
    async fn edit_replaces_only_a_unique_match() {
        let (dir, project) = git_project();
        let path = dir.path().join("a.txt");
        fs::write(&path, "alpha beta beta").unwrap();
        let edit = Edit::new(project);

        let unique = json!({"path": "a.txt", "old": "alpha", "new": "gamma"});
        edit.run(unique).await.unwrap();
        let ambiguous = json!({"path": "a.txt", "old": "beta", "new": "x"});
        let missing = json!({"path": "a.txt", "old": "delta", "new": "x"});
        let absent_file = json!({"path": "b.txt", "old": "a", "new": "x"});

        assert!(edit.run(ambiguous).await.unwrap_err().0.contains("2 times"));
        assert!(edit.run(missing).await.unwrap_err().0.contains("not found"));
        assert!(edit.run(absent_file).await.is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "gamma beta beta");
    }
}
