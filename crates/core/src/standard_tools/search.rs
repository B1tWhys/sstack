use std::fs;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ignore::WalkBuilder;
use ignore::overrides::{Override, OverrideBuilder};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::parse_input;
use super::project::Project;
use crate::connection::ToolSpec;
use crate::tools::{Tool, ToolError};
use crate::workspace::is_protected;

/// The most results returned from one search.
const MAX_RESULTS: usize = 500;

/// The tool for finding files and text in the project.
pub(super) struct Search {
    /// The project to search.
    project: Arc<Project>,
}

impl Search {
    /// Creates the tool for `project`.
    pub(super) fn new(project: Arc<Project>) -> Self {
        Self { project }
    }

    /// Lists the project's visible files, optionally limited by `glob`.
    fn walk(&self, glob: Option<&str>) -> Result<Vec<String>, ToolError> {
        let root = self.project.root();
        let owned_root = root.to_owned();
        let files = WalkBuilder::new(root)
            .hidden(false)
            .require_git(false)
            .overrides(file_filter(root, glob)?)
            .filter_entry(move |entry| {
                !entry
                    .path()
                    .strip_prefix(&owned_root)
                    .is_ok_and(is_protected)
            })
            .build()
            .flatten()
            .filter(|entry| entry.path().is_file())
            .filter_map(|entry| relative(root, entry.path()));
        Ok(files.collect())
    }
}

/// Returns `path` relative to `root`, as text.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    Some(relative.to_string_lossy().into_owned())
}

/// Builds the filter for an optional glob.
fn file_filter(root: &Path, glob: Option<&str>) -> Result<Override, ToolError> {
    let invalid = |error: ignore::Error| ToolError::Failed(format!("invalid glob: {error}"));
    let mut filter = OverrideBuilder::new(root);
    if let Some(glob) = glob {
        filter.add(glob).map_err(invalid)?;
    }
    filter.build().map_err(invalid)
}

/// Returns the lines of `file` matching `pattern`, formatted for the model.
///
/// Unreadable and binary files give no lines, rather than failing the whole search.
fn matching_lines(root: &Path, file: &str, pattern: &Regex) -> Vec<String> {
    let contents = fs::read_to_string(root.join(file)).unwrap_or_default();
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| pattern.is_match(line))
        .map(|(index, line)| format!("{file}:{}:{line}", index + 1))
        .collect()
}

/// Formats results for the model, keeping the first [`MAX_RESULTS`].
fn summarize(mut results: Vec<String>) -> String {
    if results.is_empty() {
        return "no matches".to_owned();
    }
    let total = results.len();
    results.truncate(MAX_RESULTS);
    if total > MAX_RESULTS {
        results.push(format!("... {} more", total - MAX_RESULTS));
    }
    results.join("\n")
}

/// The input to the `search` tool.
#[derive(Deserialize)]
struct SearchInput {
    /// A regex to match against each line. Without it, files are listed instead.
    pattern: Option<String>,
    /// A glob limiting which paths are searched.
    glob: Option<String>,
}

#[async_trait]
impl Tool for Search {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "Find project files. With `pattern`, return lines matching that regex \
                          as path:line:text. Without it, list file paths."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex matched against each line"},
                    "glob": {"type": "string", "description": "Only search paths matching this glob"}
                }
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let input: SearchInput = parse_input(input)?;
        let files = self.walk(input.glob.as_deref())?;
        let Some(pattern) = input.pattern else {
            return Ok(summarize(files));
        };
        let pattern =
            Regex::new(&pattern).map_err(|e| ToolError::Failed(format!("invalid pattern: {e}")))?;
        let root = self.project.root();
        let lines = files
            .iter()
            .flat_map(|file| matching_lines(root, file, &pattern));
        Ok(summarize(lines.collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standard_tools::{
        project::Project,
        test_support::{git, git_project},
    };
    use crate::workspace::{self, test_support::repository};

    fn populated() -> (tempfile::TempDir, Search) {
        let (dir, project) = git_project();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("ignored")).unwrap();
        fs::write(dir.path().join("src/a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        fs::write(dir.path().join("b.txt"), "alpha\n").unwrap();
        fs::write(dir.path().join("ignored/c.txt"), "alpha\n").unwrap();
        fs::write(dir.path().join("debug.log"), "alpha\n").unwrap();
        fs::write(dir.path().join(".env"), "alpha=secret\n").unwrap();
        (dir, Search::new(project))
    }

    #[tokio::test]
    async fn finds_matching_lines_and_skips_everything_hidden_from_the_model() {
        let (_dir, search) = populated();
        let found = search.run(json!({"pattern": "alpha"})).await.unwrap();
        let mut lines: Vec<_> = found.lines().collect();
        lines.sort_unstable();
        assert_eq!(lines, ["b.txt:1:alpha", "src/a.rs:1:fn alpha() {}"]);
    }

    #[tokio::test]
    async fn lists_files_and_respects_the_glob() {
        let (_dir, search) = populated();
        let found = search.run(json!({"glob": "*.rs"})).await.unwrap();
        assert_eq!(found, "src/a.rs");
    }

    #[tokio::test]
    async fn reports_no_matches_and_invalid_input() {
        let (_dir, search) = populated();
        let none = search.run(json!({"pattern": "zzz"})).await.unwrap();
        assert_eq!(none, "no matches");
        assert!(search.run(json!({"pattern": "("})).await.is_err());
        assert!(search.run(json!({"glob": "{"})).await.is_err());
    }

    #[tokio::test]
    async fn a_worktree_sees_its_own_files_and_the_main_checkout_does_not_see_them() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "a"]);
        let worktree = workspace::create_worktree(dir.path(), "1").unwrap();

        let from_main = Search::new(Arc::new(Project::new(dir.path()).unwrap()));
        let from_worktree = Search::new(Arc::new(Project::new(&worktree).unwrap()));
        let pattern = || json!({"pattern": "alpha"});

        assert_eq!(from_main.run(pattern()).await.unwrap(), "a.txt:1:alpha");
        assert_eq!(from_worktree.run(pattern()).await.unwrap(), "a.txt:1:alpha");
    }

    #[test]
    fn long_results_are_truncated_with_a_count() {
        let summary = summarize(vec!["x".to_owned(); MAX_RESULTS + 2]);
        assert_eq!(summary.lines().count(), MAX_RESULTS + 1);
        assert!(summary.ends_with("... 2 more"));
    }
}
