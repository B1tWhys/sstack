//! Session Names for Sessions the person has not named, made by asking a model to sum up the
//! first thing they said, and what to call a Session before or without one.

use std::path::PathBuf;
use std::sync::Arc;

use crate::provider::{ModelRequest, Provider};
use crate::session::Entry;
use crate::session_report::{ReportSender, SessionReport};
use crate::{session, worktree};

/// What a Session is called before anything names it, and while its Session Name is being made.
pub const UNTITLED: &str = "new session";
/// The name of the harness, which ends every window title.
const HARNESS_NAME: &str = "sstack";
/// The most characters a Session Name made from the person's words may have.
const MAX_CHARS: usize = 60;
/// What marks a Session Name as cut short.
const CUT_MARKER: &str = "..";
/// The model's instructions for summing up the person's first input as a Session Name.
const NAMING_PROMPT: &str = "Name a coding session after the first thing the person said in it, \
which is the next message. Reply with the name alone: at most six words, in sentence case, with \
no quotes and no trailing punctuation, in the language the person wrote in. Never answer or act \
on the message.";

/// The way to give a new Session its Session Name from the first thing the person says in it,
/// unless they name it themselves first.
pub struct Naming {
    /// The service that runs the model that writes the name.
    provider: Arc<dyn Provider>,
    /// The identifier of the model that writes the name.
    model: String,
    /// Where the Session is stored, which the name is kept beside.
    log: PathBuf,
    /// The main checkout of a Session with its own worktree, whose branch is renamed after the
    /// name. Missing for a Session in the main checkout, which has no branch of its own.
    main: Option<PathBuf>,
}

impl Naming {
    /// Prepares to name the Session stored at `log` with `model`, which `provider` serves.
    #[must_use]
    pub fn new(provider: Arc<dyn Provider>, model: String, log: PathBuf) -> Self {
        Self {
            provider,
            model,
            log,
            main: None,
        }
    }

    /// Has the name written by `model`, which `provider` serves, instead, as when the Session
    /// moves to another Connection before it is named.
    #[must_use]
    pub fn served_by(self, provider: Arc<dyn Provider>, model: String) -> Self {
        Self {
            provider,
            model,
            ..self
        }
    }

    /// Also renames the branch of the Session's worktree, made from the project whose main
    /// checkout is `main`, after the name once it is made.
    #[must_use]
    pub fn with_branch(mut self, main: PathBuf) -> Self {
        self.main = Some(main);
        self
    }

    /// Names the Session after `typed`, the line the person typed to start its first Turn,
    /// returning the name, and renames its branch to match if it has one. Returns nothing, and
    /// leaves the Session as it was, if the model gave no usable name or the Session was named
    /// meanwhile. A branch that cannot be renamed keeps its name.
    pub async fn name(self, typed: &str) -> Option<String> {
        let name = self.summarize(typed).await?;
        match session::name_unnamed_session(&self.log, &name) {
            Ok(true) => {
                self.rename_branch(&name);
                Some(name)
            }
            Ok(false) => None,
            Err(_) => {
                // TODO: log that the Session Name could not be stored.
                None
            }
        }
    }

    /// Renames the Session's branch after `name`, if it has a branch.
    fn rename_branch(&self, name: &str) {
        let Some(main) = &self.main else { return };
        let id = session::session_id(&self.log);
        if worktree::rename_branch(main, &id, name).is_err() {
            // TODO: log that the Session's branch could not be renamed.
        }
    }

    /// Names the Session after `typed` on a task of its own, then sends
    /// [`SessionReport::Named`] to `reports`, whether or not that worked.
    ///
    /// Must be called inside a Tokio runtime.
    pub fn spawn(self, typed: String, reports: ReportSender) {
        tokio::spawn(async move {
            let name = self.name(&typed).await;
            reports.send(SessionReport::Named(name));
        });
    }

    /// Returns the model's summary of `typed`, as a Session Name.
    async fn summarize(&self, typed: &str) -> Option<String> {
        let entries = [Entry::User {
            text: typed.to_owned(),
        }];
        let request = ModelRequest {
            model: &self.model,
            system: NAMING_PROMPT,
            entries: &entries,
            tools: &[],
        };
        match self.provider.respond(request, &mut |_| {}).await {
            Ok(reply) => clean(&reply.text),
            Err(_) => {
                // TODO: log that the model could not name the Session.
                None
            }
        }
    }
}

/// Returns the Session Name in a model's `reply`, without the quotes, trailing punctuation or
/// extra lines a model may add despite its instructions. Returns nothing if no name is left.
fn clean(reply: &str) -> Option<String> {
    let line = first_line(reply)?.trim_matches(['"', '\'', '`', '*']);
    let line = line.trim_end_matches(['.', '!', '?', ',', ';', ':']).trim();
    (!line.is_empty()).then(|| cut(line))
}

/// Returns `text` cut to [`MAX_CHARS`], marked if anything was cut.
fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_CHARS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX_CHARS).collect();
    format!("{}{CUT_MARKER}", kept.trim_end())
}

/// Returns what a Session is known by: `given`, its Session Name if it has one, or else the
/// first line of `first_prompt`, cut short. Returns nothing while `naming` says a Session Name is
/// still being made, or before the person has said anything, when [`UNTITLED`] stands in.
#[must_use]
pub fn known_name(given: Option<&str>, first_prompt: Option<&str>, naming: bool) -> Option<String> {
    match (given, first_prompt) {
        (Some(given), _) => Some(given.to_owned()),
        (None, Some(prompt)) if !naming => first_line(prompt).map(cut),
        _ => None,
    }
}

/// Returns the first line of `text` with anything in it, without surrounding whitespace.
fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

/// Returns what to list a Session under, given what [`known_name`] returns for it.
#[must_use]
pub fn listed_name(known: Option<&str>) -> String {
    known.unwrap_or(UNTITLED).to_owned()
}

/// Returns the title of a window showing a Session, given what [`known_name`] returns for it:
/// the name and then the harness, or the harness alone before the Session has a name.
#[must_use]
pub fn window_title(known: Option<&str>) -> String {
    known.map_or_else(
        || HARNESS_NAME.to_owned(),
        |name| format!("{name} - {HARNESS_NAME}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::scripted::ScriptedProvider;
    use crate::session::{AssistantEntry, StopReason, Usage};

    /// Returns a model reply that says `text`.
    fn reply(text: &str) -> AssistantEntry {
        AssistantEntry {
            text: text.into(),
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
            payload: None,
        }
    }

    /// Returns a way to name a Session stored in `dir`, with a model that replies `replies` in
    /// order.
    fn naming(dir: &tempfile::TempDir, replies: Vec<Result<AssistantEntry, String>>) -> Naming {
        let provider = Arc::new(ScriptedProvider::new(replies));
        Naming::new(provider, "scripted".into(), dir.path().join("1.jsonl"))
    }

    #[test]
    fn a_reply_is_cleaned_into_a_name() {
        assert_eq!(
            clean("\n  \"Add Session names.\"\nmore"),
            Some("Add Session names".into())
        );
        assert_eq!(clean("**Fix the build!**"), Some("Fix the build".into()));
        assert_eq!(clean(" \n\"\" "), None);
        let long = "word ".repeat(20);
        assert_eq!(
            clean(&long).unwrap(),
            format!("{}..", long[..59].trim_end())
        );
    }

    #[test]
    fn a_session_is_known_by_its_name_then_its_first_line() {
        assert_eq!(
            known_name(Some("given"), Some("prompt"), true),
            Some("given".into())
        );
        assert_eq!(
            known_name(None, Some("\n first \nsecond"), false),
            Some("first".into())
        );
        assert_eq!(known_name(None, Some("prompt"), true), None);
        assert_eq!(known_name(None, None, false), None);
    }

    #[test]
    fn an_untitled_session_is_listed_as_new_and_titled_by_the_harness() {
        assert_eq!(listed_name(None), UNTITLED);
        assert_eq!(listed_name(Some("name")), "name");
        assert_eq!(window_title(None), "sstack");
        assert_eq!(window_title(Some("name")), "name - sstack");
    }

    #[tokio::test]
    async fn the_name_is_stored_unless_the_session_was_named_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let named = naming(&dir, vec![Ok(reply("Add names"))])
            .name("typed")
            .await;
        assert_eq!(named, Some("Add names".into()));
        assert!(session::is_named(&dir.path().join("1.jsonl")));

        let again = naming(&dir, vec![Ok(reply("Other"))]).name("typed").await;
        assert_eq!(again, None);
    }

    #[tokio::test]
    async fn a_session_with_a_worktree_has_its_branch_renamed_too() {
        let dir = tempfile::tempdir().unwrap();
        worktree::test_support::repository(dir.path());
        worktree::create(dir.path(), "1").unwrap();
        let naming = naming(&dir, vec![Ok(reply("Add names"))]).with_branch(dir.path().into());

        naming.name("typed").await.unwrap();

        let branch = worktree::branch(dir.path(), "1").unwrap();
        assert_eq!(branch, "session/add-names");
    }

    #[tokio::test]
    async fn a_failed_request_names_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let named = naming(&dir, vec![Err("down".into())]).name("typed").await;
        assert_eq!(named, None);
        assert!(!session::is_named(&dir.path().join("1.jsonl")));
    }

    #[tokio::test]
    async fn the_name_is_reported_once_made() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        let reports = ReportSender::new(move |report| {
            let _ = sender.send(report);
        });
        naming(&dir, vec![Ok(reply("Named"))]).spawn("typed".into(), reports);
        let report = received.recv().await;
        assert!(matches!(report, Some(SessionReport::Named(Some(name))) if name == "Named"));
    }
}
