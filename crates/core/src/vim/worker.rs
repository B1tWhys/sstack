//! Keeping an nvim running on behalf of a [`Vim`](super::Vim).

use std::io;
use std::path::PathBuf;
use std::pin::pin;
use std::process::ExitStatus;
use std::time::Duration;

use bon::Builder;
use rmpv::Value;
use tokio::sync::mpsc;
use tokio::time::sleep;

use super::connection::{Connection, Inbound};
use super::{Command, Draft, VimEvent, VimFailure};

/// The script that prepares nvim to edit a [`Draft`].
const BOOTSTRAP: &str = include_str!("bootstrap.lua");

/// Goes to Normal mode whatever was half-typed, then hands over the text. It travels with the keys
/// so that it lands after them, and cancelling frees nvim if it was waiting on a key.
const SUBMIT_KEYS: &str = "<Esc><Cmd>lua sstack_submit()<CR>";

/// Why one nvim stopped being used.
enum Ended {
    /// The front end is gone, so nothing needs nvim any more.
    Abandoned,
    /// The person quit nvim on purpose.
    Quit,
    /// Something went wrong that another nvim would not fix.
    Failed(VimFailure),
}

/// Runs nvim until its front end goes away, replacing it if the person quits it.
#[derive(Builder)]
pub struct Worker {
    /// What the front end wants done.
    inbox: mpsc::UnboundedReceiver<Command>,
    /// Tells the front end what happened.
    report: Box<dyn FnMut(VimEvent) + Send>,
    /// The text as last reported, for the next nvim or for the front end to recover.
    draft: Draft,
    /// The program to run.
    program: PathBuf,
    /// Whether to ignore the person's config.
    clean: bool,
    /// How long nvim gets to start.
    ready_timeout: Duration,
    /// Whether the current nvim has finished starting.
    #[builder(skip)]
    ready: bool,
}

impl Worker {
    /// Works until the front end goes away or nvim is lost for good.
    pub async fn run(mut self) {
        loop {
            match self.run_one().await {
                Ended::Abandoned => return,
                Ended::Quit => {}
                Ended::Failed(failure) => {
                    let draft = std::mem::take(&mut self.draft);
                    (self.report)(VimEvent::Lost { draft, failure });
                    return;
                }
            }
        }
    }

    /// Starts one nvim and runs it until it is no longer used.
    async fn run_one(&mut self) -> Ended {
        match self.start().await {
            Ok(nvim) => self.drive(nvim).await,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ended::Failed(VimFailure::NotInstalled)
            }
            Err(error) => Ended::Failed(VimFailure::Unreachable(error.to_string())),
        }
    }

    /// Starts nvim and sets it up to edit the draft.
    async fn start(&mut self) -> io::Result<Connection> {
        let mut nvim = Connection::open(&self.program, self.clean)?;
        let arguments = vec![
            Value::from(self.draft.text.as_str()),
            Value::from(self.draft.cursor),
        ];
        let params = vec![Value::from(BOOTSTRAP), Value::from(arguments)];
        nvim.notify("nvim_exec_lua", params).await?;
        Ok(nvim)
    }

    /// Passes messages both ways until nvim stops being used.
    ///
    /// The front end's messages wait until nvim has reported in, because keys overtake anything
    /// sent to nvim before them, including its setup.
    async fn drive(&mut self, mut nvim: Connection) -> Ended {
        self.ready = false;
        let mut deadline = pin!(sleep(self.ready_timeout));
        loop {
            tokio::select! {
                command = self.inbox.recv(), if self.ready => {
                    let Some(command) = command else {
                        return Ended::Abandoned;
                    };
                    // A failed send means nvim has gone, which reading from it will discover.
                    let _ = forward(&mut nvim, command).await;
                }
                inbound = nvim.next() => {
                    if let Some(ended) = self.receive(&mut nvim, inbound).await {
                        return ended;
                    }
                }
                () = &mut deadline, if !self.ready => return Ended::Failed(VimFailure::TimedOut),
            }
        }
    }

    /// Reacts to something nvim said, returning why nvim is finished if it is.
    async fn receive(
        &mut self,
        nvim: &mut Connection,
        inbound: io::Result<Inbound>,
    ) -> Option<Ended> {
        let inbound = match inbound {
            Ok(inbound) => inbound,
            Err(error) => return Some(Ended::Failed(VimFailure::Unreachable(error.to_string()))),
        };
        match inbound {
            Inbound::State(state) => {
                self.ready = true;
                self.draft = Draft::from(&*state);
                (self.report)(VimEvent::State(*state));
            }
            Inbound::Submitted(text) => (self.report)(VimEvent::Submitted(text)),
            Inbound::Error(error) if !self.ready => {
                return Some(Ended::Failed(VimFailure::Rejected(error)));
            }
            Inbound::Error(_) | Inbound::Other => {}
            Inbound::Closed => return Some(how_it_ended(nvim.wait().await, self.ready)),
        }
        None
    }
}

/// Sends one of the front end's messages on to nvim.
async fn forward(nvim: &mut Connection, command: Command) -> io::Result<()> {
    match command {
        Command::Keys(keys) => input(nvim, keys).await,
        Command::Submit => input(nvim, SUBMIT_KEYS.to_owned()).await,
        Command::Paste(text) => {
            let params = vec![
                Value::from(text),
                Value::from(false),
                Value::from(WHOLE_PASTE),
            ];
            nvim.notify("nvim_paste", params).await
        }
        Command::Resize { rows, columns } => {
            set_size(nvim, "lines", rows).await?;
            set_size(nvim, "columns", columns).await
        }
    }
}

/// The paste phase meaning that this one call is the whole paste.
const WHOLE_PASTE: i64 = -1;

/// Queues keys as if typed.
async fn input(nvim: &mut Connection, keys: String) -> io::Result<()> {
    nvim.notify("nvim_input", vec![Value::from(keys)]).await
}

/// Sets the screen size option `name`. Nvim refuses anything too small to work with, so a tiny area
/// leaves it at the smallest size it allows.
async fn set_size(nvim: &mut Connection, name: &str, size: u16) -> io::Result<()> {
    let params = vec![Value::from(name), Value::from(size), Value::Map(Vec::new())];
    nvim.notify("nvim_set_option_value", params).await
}

/// Decides what nvim closing its end means, given how it exited and whether it had finished
/// starting. A clean exit before that is a failure, because a new nvim would do the same.
fn how_it_ended(status: io::Result<ExitStatus>, ready: bool) -> Ended {
    match status {
        Ok(status) if status.success() && ready => Ended::Quit,
        Ok(status) => Ended::Failed(VimFailure::Stopped(status.to_string())),
        Err(error) => Ended::Failed(VimFailure::Unreachable(error.to_string())),
    }
}
