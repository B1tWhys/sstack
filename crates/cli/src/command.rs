//! Telling Human Commands apart from input meant for the model.

/// What a line entered by the person asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    /// Text to send to the model.
    Prompt(&'a str),
    /// The `/rebuild` Human Command.
    Rebuild,
    /// The `/new` Human Command, also spelled `/n`, which starts a Session in its own worktree.
    New,
    /// The `/new-main` Human Command, which starts a Session in the main checkout.
    NewMain,
    /// The `/archive` Human Command, which puts the Session on screen away, deleting its worktree.
    Archive {
        /// Whether to go ahead even if that deletes work.
        force: bool,
    },
    /// The `/vim` Human Command, which switches the Session on screen between typing into a real
    /// nvim and the plain prompt.
    Vim,
    /// The `/quit` Human Command, also spelled `/exit`.
    Quit,
    /// A slash-prefixed line that is not a known Human Command.
    Unknown(&'a str),
    /// A blank line.
    Empty,
}

impl Command<'_> {
    /// Returns whether this starts or removes work, which has no place while the harness is on
    /// its way out.
    #[must_use]
    pub fn changes_work(&self) -> bool {
        matches!(
            self,
            Self::Prompt(_) | Self::Rebuild | Self::New | Self::NewMain | Self::Archive { .. }
        )
    }
}

/// Works out what a line entered by the person asks for.
///
/// Slash-prefixed lines never reach the model, even if the command is mistyped.
#[must_use]
pub fn parse_command(line: &str) -> Command<'_> {
    match line.trim() {
        "" => Command::Empty,
        "/rebuild" => Command::Rebuild,
        "/new" | "/n" => Command::New,
        "/new-main" => Command::NewMain,
        "/archive" => Command::Archive { force: false },
        "/archive --force" => Command::Archive { force: true },
        "/vim" => Command::Vim,
        "/quit" | "/exit" => Command::Quit,
        other if other.starts_with('/') => Command::Unknown(other),
        other => Command::Prompt(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_parse_into_commands() {
        assert_eq!(parse_command(" hello "), Command::Prompt("hello"));
        assert_eq!(parse_command("/rebuild"), Command::Rebuild);
        assert_eq!(parse_command("/new"), Command::New);
        assert_eq!(parse_command("/n"), Command::New);
        assert_eq!(parse_command("/new-main"), Command::NewMain);
        assert_eq!(parse_command("/archive"), Command::Archive { force: false });
        assert_eq!(
            parse_command("/archive --force"),
            Command::Archive { force: true }
        );
        assert_eq!(parse_command("/vim"), Command::Vim);
        assert_eq!(parse_command("/quit"), Command::Quit);
        assert_eq!(parse_command("/exit"), Command::Quit);
        assert_eq!(parse_command("/nope"), Command::Unknown("/nope"));
        assert_eq!(parse_command("   "), Command::Empty);
    }

    #[test]
    fn only_some_commands_change_work() {
        assert!(Command::Prompt("hi").changes_work());
        assert!(Command::New.changes_work());
        assert!(Command::Archive { force: true }.changes_work());
        assert!(Command::Rebuild.changes_work());
        assert!(!Command::Quit.changes_work());
    }
}
