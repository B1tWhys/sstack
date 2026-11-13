//! The instructions a Session starts with: the system prompt, and what is added to it for a
//! worktree or for the Skills the model can use.

use std::fs;
use std::path::Path;

use crate::commands::Skill;

/// The instructions every Session starts with.
const BASE_PROMPT: &str = "You are sstack, a coding agent working in the user's project. \
Use the tools to inspect and change the project, and verify your changes before finishing. \
Be direct, and report results faithfully, including anything that failed.";
/// The file a project keeps its own instructions in.
const INSTRUCTIONS_FILE: &str = "AGENTS.md";
/// The file, in Claude Code's directory, a person keeps instructions for every project in.
const USER_INSTRUCTIONS_FILE: &str = "CLAUDE.md";
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
    use crate::commands::Scope;

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
