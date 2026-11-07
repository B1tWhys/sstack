use std::fs;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolError, parse_input};
use crate::provider::ToolSpec;
use crate::skills::Skill;

/// Lets the model read the instructions of an installed [`Skill`], which may live outside the
/// project, without opening anything else out there.
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
        ToolSpec {
            name: "skill".into(),
            description: "Read the instructions of a Skill listed in the system prompt.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The Skill's name"}
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
        fs::read_to_string(&skill.path)
            .map_err(|error| ToolError(format!("{}: {error}", input.name)))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::skills::Scope;

    /// Returns a Skill called `name` whose instructions are at `path`.
    fn skill(name: &str, path: &Path) -> Skill {
        Skill {
            name: name.into(),
            description: String::new(),
            path: path.to_path_buf(),
            scope: Scope::Project,
        }
    }

    #[tokio::test]
    async fn returns_the_instructions_of_an_offered_skill() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SKILL.md");
        fs::write(&path, "Write short commits.").unwrap();
        let tool = UseSkill::new(vec![skill("commits", &path)]);

        let text = tool.run(json!({"name": "commits"})).await.unwrap();

        assert_eq!(text, "Write short commits.");
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
