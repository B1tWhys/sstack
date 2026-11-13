//! Text editing driven by a real nvim, for any front end to draw.
//!
//! Nvim does the editing, so a person's own mappings, options and plugins all work. A front end
//! only passes keys in and draws the [`VimState`] that comes back. It never computes an edit.

mod connection;
pub mod layout;
mod worker;

use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use bon::bon;
use serde::Deserialize;
use tokio::sync::mpsc;

use worker::Worker;

/// The program started unless a [`Vim`] asks for another. Found on the `PATH`.
const DEFAULT_PROGRAM: &str = "nvim";

/// How long nvim gets to start before it counts as failed. Generous, because a person's config may
/// spend a while on plugins.
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Text not yet submitted, with where its cursor belongs, in a form that can start a [`Vim`] and
/// survive losing one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// The text, with lines separated by newlines.
    pub text: String,
    /// Where the cursor sits, as a byte offset into `text` on a character boundary.
    pub cursor: usize,
}

/// Which family of modes nvim is in, which is what a front end needs for its mode label and
/// cursor shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum VimMode {
    /// Normal mode, including while an operator waits for its motion.
    Normal,
    /// Insert mode.
    Insert,
    /// Replace mode.
    Replace,
    /// Any kind of Visual or Select mode.
    Visual,
    /// Command-line mode, as after `:` or `/`.
    Command,
}

impl From<String> for VimMode {
    fn from(name: String) -> Self {
        match name.as_str() {
            "insert" => Self::Insert,
            "replace" => Self::Replace,
            "visual" => Self::Visual,
            "command" => Self::Command,
            _ => Self::Normal,
        }
    }
}

/// A command line being typed, for a front end to draw because nvim draws nothing itself.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VimCmdline {
    /// What to show, starting with the `:` or `/` that opened it.
    pub text: String,
    /// Where the cursor sits, as a byte offset into `text`.
    pub cursor: usize,
}

/// Everything a front end needs to draw the text being edited at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VimState {
    /// The text, with lines separated by newlines.
    pub text: String,
    /// Where the cursor sits, as a byte offset into `text`. In Insert mode that is between two
    /// characters, and in other modes it is on the character starting there.
    pub cursor: usize,
    /// Which mode nvim is in.
    pub mode: VimMode,
    /// What is selected, as byte ranges into `text`. A selection spanning lines gets one range per
    /// line, and is empty outside Visual mode.
    pub selection: Vec<Range<usize>>,
    /// What nvim's number options put beside each line of `text`, in order and all the same
    /// width, spacing included. Empty if they are off. Nvim leaves that width out of the width it
    /// wraps at, so a front end drawing these should give [`Vim::resize`] the width with them
    /// included.
    #[serde(default)]
    pub line_numbers: Vec<String>,
    /// The command line, if one is being typed.
    #[serde(default)]
    pub cmdline: Option<VimCmdline>,
    /// The latest error, which lasts until the next key.
    #[serde(default)]
    pub message: Option<String>,
}

impl From<&VimState> for Draft {
    fn from(state: &VimState) -> Self {
        Self {
            text: state.text.clone(),
            cursor: state.cursor,
        }
    }
}

/// Why a [`Vim`] stopped working for good.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VimFailure {
    /// No nvim could be found to start.
    #[error("nvim is not installed")]
    NotInstalled,
    /// Nvim could not be started or talked to.
    #[error("nvim could not be reached: {0}")]
    Unreachable(String),
    /// Nvim exited while starting, or crashed later. Quitting on purpose is not a failure.
    #[error("nvim stopped: {0}")]
    Stopped(String),
    /// Nvim did not finish starting in time.
    #[error("nvim took too long to start")]
    TimedOut,
    /// Nvim started but could not be set up, usually because it is too old.
    #[error("nvim could not be set up: {0}")]
    Rejected(String),
}

/// Something a [`Vim`] reports to its front end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VimEvent {
    /// What is being edited changed in some way. Replaces any earlier state.
    State(VimState),
    /// The person submitted this text, in response to [`Vim::submit`].
    Submitted(String),
    /// Nvim is gone for good and no more events will come. The front end should fall back to
    /// another way of typing, starting from `draft`.
    Lost {
        /// The text as nvim last reported it, which may miss the final key or two.
        draft: Draft,
        /// What went wrong.
        failure: VimFailure,
    },
}

/// One message for nvim from the front end.
#[derive(Debug)]
enum Command {
    /// Keys to feed in, in nvim's key notation.
    Keys(String),
    /// Text to paste in one piece.
    Paste(String),
    /// The new size of the area the text is drawn in.
    Resize {
        /// The height in rows.
        rows: u16,
        /// The width in columns.
        columns: u16,
    },
    /// A request to hand over the text and start again with an empty buffer.
    Submit,
}

/// An editor for one piece of text, with a real nvim running behind the scenes that stays alive
/// for as long as this value exists.
///
/// Nothing here waits, so a front end can call it from its input handling. What happens comes back
/// through the `on_event` callback, in order, from another task. Everything sent before nvim has
/// finished starting is held back until it has.
///
/// If the person quits nvim with `:q` or `ZZ`, a new one takes over with the same text, losing only
/// undo history and the mode. Any other exit is reported as [`VimEvent::Lost`].
#[derive(Debug)]
pub struct Vim {
    /// Where messages for nvim are queued, which keeps them in order.
    commands: mpsc::UnboundedSender<Command>,
}

#[bon]
impl Vim {
    /// Starts nvim in the background, in Insert mode. Must be called on a tokio runtime.
    ///
    /// Failing to start is not an error here, but a [`VimEvent::Lost`] like any other failure.
    #[builder(finish_fn = start)]
    pub fn new(
        /// Receives everything the [`Vim`] reports.
        on_event: impl FnMut(VimEvent) + Send + 'static,
        /// What to start with, instead of an empty buffer. Loaded like an opened file, so undo
        /// cannot remove it.
        #[builder(default)]
        draft: Draft,
        /// The program to run, for when `nvim` on the `PATH` is not the right one.
        #[builder(default = DEFAULT_PROGRAM.into())]
        program: PathBuf,
        /// Whether to ignore the person's config, so that behavior is the same for everyone.
        #[builder(default)]
        clean: bool,
        /// How long nvim gets to start.
        #[builder(default = DEFAULT_READY_TIMEOUT)]
        ready_timeout: Duration,
    ) -> Self {
        let (commands, inbox) = mpsc::unbounded_channel();
        let worker = Worker::builder()
            .inbox(inbox)
            .report(Box::new(on_event))
            .draft(draft)
            .program(program)
            .clean(clean)
            .ready_timeout(ready_timeout)
            .build();
        tokio::spawn(worker.run());
        Self { commands }
    }
}

impl Vim {
    /// Feeds in keys as if typed, written in nvim's key notation, such as `dw` or `<C-a>`. A
    /// literal `<` must be written as `<lt>`.
    pub fn send_keys(&self, keys: &str) {
        self.send(Command::Keys(keys.to_owned()));
    }

    /// Inserts `text` in one piece, however long it is and whichever mode nvim is in.
    pub fn paste(&self, text: &str) {
        self.send(Command::Paste(text.to_owned()));
    }

    /// Tells nvim how big the text area is, so that motions like `gj` and `<C-d>` match the
    /// screen. A front end with proportional fonts will have to approximate.
    pub fn resize(&self, rows: u16, columns: u16) {
        self.send(Command::Resize { rows, columns });
    }

    /// Asks for the text, which arrives as a [`VimEvent::Submitted`] after every key sent before
    /// this has taken effect. Cancels anything half-typed, then leaves an empty buffer in Insert
    /// mode, where undo can still bring the text back.
    pub fn submit(&self) {
        self.send(Command::Submit);
    }

    /// Queues `command`. It goes nowhere if nvim is lost, which the front end hears about anyway.
    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

#[cfg(test)]
mod tests;
