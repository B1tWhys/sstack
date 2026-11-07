use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolError, parse_input};
use crate::provider::ToolSpec;
use crate::skills::{self, Skill};

/// The most files beside a `SKILL.md` that the tool names, so that a Skill with a large tree of
/// resources does not flood the model.
const MAX_LISTED_FILES: usize = 50;

/// Lets the model load the instructions of an installed [`Skill`], which may live outside the
/// project, and learn which files sit beside them, without opening anything else out there.
pub(super) struct UseSkill {
    /// The only Skills the model can ask for.
    skills: Vec<Skill>,
}

impl UseSkill {
    /// Creates the tool, offering `skills`.
    pub(super) fn new(skills: Vec<Skill>) -> Self {
        Self { skills }
    }
}

/// The input to the `skill` tool.
#[derive(Deserialize)]
struct SkillInput {
    /// The Skill wanted, as named in the system prompt.
    name: String,
}

#[async_trait]
impl Tool for UseSkill {
    fn spec(&self) -> ToolSpec {
        let names: Vec<&str> = self
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        ToolSpec {
            name: "skill".into(),
            description: "Load the instructions of a Skill listed in the system prompt, with the \
                paths of the files beside them, which `read` can open."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "enum": names, "description": "The Skill's name"}
                },
                "required": ["name"]
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let input: SkillInput = parse_input(input)?;
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.name == input.name)
            .ok_or_else(|| ToolError(format!("unknown skill: {}", input.name)))?;
        let text = fs::read_to_string(&skill.path)
            .map_err(|error| ToolError(format!("{}: {error}", input.name)))?;
        let directory = skill.path.parent().unwrap_or(Path::new(""));
        Ok(loaded(skill, directory, &text))
    }
}

/// Describes `skill` to the model once loaded: its instructions from `text`, then the files in
/// its `directory` other than `SKILL.md`, if there are any.
fn loaded(skill: &Skill, directory: &Path, text: &str) -> String {
    let mut out = format!(
        "<skill name=\"{}\" directory=\"{}\">\n{}\n</skill>",
        skill.name,
        directory.display(),
        skills::instructions(text).trim_end()
    );
    let files = files_beside(directory, &skill.path);
    if files.is_empty() {
        return out;
    }
    let mut lines: Vec<String> = files
        .iter()
        .take(MAX_LISTED_FILES)
        .map(|file| format!("- {}", file.display()))
        .collect();
    if files.len() > MAX_LISTED_FILES {
        lines.push(format!("- and {} more", files.len() - MAX_LISTED_FILES));
    }
    out.push_str("\n\nFiles beside the instructions:\n");
    out.push_str(&lines.join("\n"));
    out
}

/// Returns the files under `directory` other than `instructions`, in a stable order, leaving out
/// hidden and ignored files.
fn files_beside(directory: &Path, instructions: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(directory)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|path| path != instructions)
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::Scope;

    /// Returns a Skill called `name` whose instructions are at `path`.
    fn skill(name: &str, path: &Path) -> Skill {
        Skill {
            name: name.into(),
            description: String::new(),
            path: path.to_path_buf(),
            scope: Scope::Project,
            model_invocable: true,
        }
    }

    #[tokio::test]
    async fn returns_the_instructions_without_their_header_and_the_files_beside_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SKILL.md");
        fs::write(&path, "---\nname: commits\n---\nWrite short commits.\n").unwrap();
        fs::create_dir(dir.path().join("references")).unwrap();
        fs::write(dir.path().join("references/style.md"), "").unwrap();
        let tool = UseSkill::new(vec![skill("commits", &path)]);

        let text = tool.run(json!({"name": "commits"})).await.unwrap();

        let directory = dir.path().display();
        let expected = format!(
            "<skill name=\"commits\" directory=\"{directory}\">\nWrite short commits.\n</skill>\
             \n\nFiles beside the instructions:\n- {directory}/references/style.md"
        );
        assert_eq!(text, expected);
    }

    #[tokio::test]
    async fn a_skill_with_nothing_beside_it_lists_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SKILL.md");
        fs::write(&path, "Alone.").unwrap();
        let tool = UseSkill::new(vec![skill("alone", &path)]);

        let text = tool.run(json!({"name": "alone"})).await.unwrap();

        assert!(text.ends_with("Alone.\n</skill>"), "{text}");
    }

    #[test]
    fn a_long_list_of_files_is_cut_short() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SKILL.md");
        for index in 0..=MAX_LISTED_FILES {
            fs::write(dir.path().join(format!("{index:03}.md")), "").unwrap();
        }

        let text = loaded(&skill("many", &path), dir.path(), "Many.");

        assert!(text.ends_with("\n- and 1 more"), "{text}");
    }

    #[test]
    fn only_offered_skills_can_be_named() {
        let tool = UseSkill::new(vec![skill("a", Path::new("a")), skill("b", Path::new("b"))]);
        let spec = tool.spec();
        assert_eq!(
            spec.input_schema["properties"]["name"]["enum"],
            json!(["a", "b"])
        );
    }

    #[tokio::test]
    async fn refuses_a_skill_it_does_not_offer() {
        let tool = UseSkill::new(vec![]);
        let error = tool.run(json!({"name": "../secret"})).await.unwrap_err();
        assert_eq!(error, ToolError("unknown skill: ../secret".into()));
    }

    #[tokio::test]
    async fn reports_a_skill_that_can_no_longer_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let tool = UseSkill::new(vec![skill("gone", &dir.path().join("SKILL.md"))]);
        let error = tool.run(json!({"name": "gone"})).await.unwrap_err();
        assert!(error.0.starts_with("gone: "), "{error}");
    }
}
