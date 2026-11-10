//! Human Commands, told apart from input meant for the model.

use crate::rebuild::Checkout;

/// What a line entered by the person asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    /// Text to send to the model.
    Prompt(&'a str),
    /// The `/rebuild` Human Command, which builds the main checkout, or with `--worktree` the
    /// Session's own worktree.
    Rebuild(Checkout),
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
    /// The `/rename` Human Command, which makes the text after it the Session Name of the Session
    /// on screen. With nothing after it, the Session is known by its first prompt again.
    Rename(Option<&'a str>),
    /// The `/connection` Human Command, which moves the Session on screen to the Connection
    /// named after it from its next Turn on. With nothing after it, the interface offers the
    /// Connections to choose from.
    Connection(Option<&'a str>),
    /// The `/quit` Human Command, also spelled `/exit`.
    Quit,
    /// A slash-prefixed line that is not a known Human Command.
    Unknown(&'a str),
    /// A blank line.
    Empty,
}

impl Command<'_> {
    /// Returns whether this starts or removes work, which has no place while the Running Harness is on
    /// its way out.
    #[must_use]
    pub fn changes_work(&self) -> bool {
        matches!(
            self,
            Self::Prompt(_) | Self::Rebuild(_) | Self::New | Self::NewMain | Self::Archive { .. }
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
        "/rebuild" => Command::Rebuild(Checkout::Main),
        "/rebuild --worktree" => Command::Rebuild(Checkout::Worktree),
        "/new" | "/n" => Command::New,
        "/new-main" => Command::NewMain,
        "/archive" => Command::Archive { force: false },
        "/archive --force" => Command::Archive { force: true },
        "/vim" => Command::Vim,
        "/quit" | "/exit" => Command::Quit,
        other if other.starts_with('/') => with_argument(other),
        other => Command::Prompt(other),
    }
}

/// Works out what a slash-prefixed `line` asks for when it is not a Human Command on its own,
/// which leaves the Human Commands that take text after their name.
fn with_argument(line: &str) -> Command<'_> {
    let (name, argument) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    match name {
        "/rename" => Command::Rename(Some(argument.trim()).filter(|name| !name.is_empty())),
        "/connection" => Command::Connection(Some(argument.trim()).filter(|name| !name.is_empty())),
        _ => Command::Unknown(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_parse_into_commands() {
        assert_eq!(parse_command(" hello "), Command::Prompt("hello"));
        assert_eq!(parse_command("/rebuild"), Command::Rebuild(Checkout::Main));
        assert_eq!(
            parse_command("/rebuild --worktree"),
            Command::Rebuild(Checkout::Worktree)
        );
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
        assert_eq!(
            parse_command("/nope at all"),
            Command::Unknown("/nope at all")
        );
        assert_eq!(parse_command("   "), Command::Empty);
    }

    #[test]
    fn rename_takes_the_rest_of_the_line_as_the_name() {
        let renamed = parse_command("/rename  fix the  sidebar ");
        assert_eq!(renamed, Command::Rename(Some("fix the  sidebar")));
        assert_eq!(parse_command("/rename"), Command::Rename(None));
        assert_eq!(parse_command("/rename   "), Command::Rename(None));
        assert_eq!(parse_command("/renamed"), Command::Unknown("/renamed"));
    }

    #[test]
    fn connection_takes_the_name_after_it() {
        assert_eq!(
            parse_command("/connection  openai "),
            Command::Connection(Some("openai"))
        );
        assert_eq!(parse_command("/connection"), Command::Connection(None));
    }

    #[test]
    fn only_some_commands_change_work() {
        assert!(Command::Prompt("hi").changes_work());
        assert!(Command::New.changes_work());
        assert!(Command::Archive { force: true }.changes_work());
        assert!(Command::Rebuild(Checkout::Worktree).changes_work());
        assert!(!Command::Quit.changes_work());
    }
}
