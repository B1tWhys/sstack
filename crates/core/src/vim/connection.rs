//! A msgpack-RPC connection to one nvim process.

use std::io::{self, Cursor};
use std::path::Path;
use std::process::{ExitStatus, Stdio};

use rmpv::Value;
use rmpv::ext::{from_value, to_value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::VimState;

/// How msgpack-RPC marks a message that expects no reply.
const NOTIFICATION: u8 = 2;

/// The marker set before the person's config runs, so that it can skip what does not belong in a
/// prompt.
const MARK_AS_SSTACK: &str = "let g:sstack = 1";

/// How many bytes to read from nvim at a time.
const READ_SIZE: usize = 8192;

/// Something nvim told the harness.
#[derive(Debug)]
pub enum Inbound {
    /// What is being edited changed.
    State(Box<VimState>),
    /// The person submitted this text, and nvim emptied the buffer.
    Submitted(String),
    /// Nvim could not carry out something it was sent.
    Error(String),
    /// Something nothing here reacts to.
    Other,
    /// Nvim closed its end, which means it has exited or is about to.
    Closed,
}

/// A connection to a running nvim, which kills the nvim when dropped.
///
/// Only notifications are ever sent. Nvim would not answer a request while it waits on a key, and
/// the harness must stay free to send that key.
#[derive(Debug)]
pub struct Connection {
    /// The running nvim.
    child: Child,
    /// Where messages for nvim go.
    stdin: ChildStdin,
    /// Where messages from nvim arrive.
    stdout: ChildStdout,
    /// Bytes from nvim that do not yet add up to a whole message.
    unread: Vec<u8>,
}

impl Connection {
    /// Starts `program` as an embedded nvim, with the person's config unless `clean` is set.
    ///
    /// # Errors
    /// Returns an error if the program cannot be started.
    pub fn open(program: &Path, clean: bool) -> io::Result<Self> {
        let mut command = Command::new(program);
        command.args(["--embed", "--headless", "-n", "--cmd", MARK_AS_SSTACK]);
        command.args(clean.then_some("--clean"));
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(missing_pipe)?;
        let stdout = child.stdout.take().ok_or_else(missing_pipe)?;
        Ok(Self {
            child,
            stdin,
            stdout,
            unread: Vec::new(),
        })
    }

    /// Calls the API function `method` without waiting for it to finish.
    ///
    /// # Errors
    /// Returns an error if nvim can no longer be reached.
    pub async fn notify(&mut self, method: &str, params: Vec<Value>) -> io::Result<()> {
        let message = to_value((NOTIFICATION, method, params)).map_err(io::Error::other)?;
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &message).map_err(io::Error::other)?;
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await
    }

    /// Waits for the next thing nvim says. Safe to cancel, because nothing read is lost.
    ///
    /// # Errors
    /// Returns an error if nvim's output cannot be read or makes no sense.
    pub async fn next(&mut self) -> io::Result<Inbound> {
        loop {
            if let Some(message) = take_message(&mut self.unread)? {
                return Ok(parse(message));
            }
            let mut chunk = [0; READ_SIZE];
            let read = self.stdout.read(&mut chunk).await?;
            if read == 0 {
                return Ok(Inbound::Closed);
            }
            self.unread.extend_from_slice(&chunk[..read]);
        }
    }

    /// Waits for nvim to exit, once it has closed its end, and returns how it did.
    ///
    /// # Errors
    /// Returns an error if the process cannot be waited for.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait().await
    }
}

/// Returns the error for a pipe that was asked for but is not there, which should not happen.
fn missing_pipe() -> io::Error {
    io::Error::other("nvim started without its pipes")
}

/// Removes the first whole message from `bytes`, if there is one yet.
fn take_message(bytes: &mut Vec<u8>) -> io::Result<Option<Value>> {
    let mut reader = Cursor::new(bytes.as_slice());
    match rmpv::decode::read_value(&mut reader) {
        Ok(value) => {
            let used = usize::try_from(reader.position()).map_err(io::Error::other)?;
            bytes.drain(..used);
            Ok(Some(value))
        }
        Err(error) if is_incomplete(&error) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

/// Tells whether a decoding failure only means that more bytes are needed.
fn is_incomplete(error: &rmpv::decode::Error) -> bool {
    use rmpv::decode::Error::{DepthLimitExceeded, InvalidDataRead, InvalidMarkerRead};
    match error {
        InvalidMarkerRead(error) | InvalidDataRead(error) => {
            error.kind() == io::ErrorKind::UnexpectedEof
        }
        DepthLimitExceeded => false,
    }
}

/// Works out what a message from nvim means.
fn parse(message: Value) -> Inbound {
    let Ok((NOTIFICATION, method, params)) = from_value::<(u8, String, Vec<Value>)>(message) else {
        return Inbound::Other;
    };
    let last = params.into_iter().next_back().unwrap_or(Value::Nil);
    match method.as_str() {
        "sstack_state" => {
            from_value(last).map_or(Inbound::Other, |state| Inbound::State(Box::new(state)))
        }
        "sstack_submitted" => from_value(last).map_or(Inbound::Other, Inbound::Submitted),
        "nvim_error_event" => Inbound::Error(last.to_string()),
        _ => Inbound::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes `value` the way nvim would send it.
    fn encoded(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, value).unwrap();
        bytes
    }

    /// Returns a notification from nvim.
    fn notification(method: &str, last: Value) -> Value {
        to_value((NOTIFICATION, method, vec![last])).unwrap()
    }

    #[test]
    fn a_message_is_taken_only_once_all_of_it_has_arrived() {
        let bytes = encoded(&Value::from("hello"));
        let (start, rest) = bytes.split_at(3);
        let mut unread = start.to_vec();
        assert_eq!(take_message(&mut unread).unwrap(), None);
        unread.extend_from_slice(rest);
        unread.push(1);
        assert_eq!(
            take_message(&mut unread).unwrap(),
            Some(Value::from("hello"))
        );
        assert_eq!(unread, [1]);
    }

    #[test]
    fn an_empty_buffer_holds_no_message() {
        assert_eq!(take_message(&mut Vec::new()).unwrap(), None);
    }

    #[test]
    fn output_nested_absurdly_deep_is_an_error() {
        let decode = || {
            let mut bytes = vec![0x91; 4096];
            bytes.push(0);
            assert!(take_message(&mut bytes).is_err());
        };
        // Debug builds need more stack than a test thread gets to reach the depth limit.
        let thread = std::thread::Builder::new().stack_size(1 << 28);
        thread.spawn(decode).unwrap().join().unwrap();
    }

    #[test]
    fn messages_parse_into_what_they_mean() {
        let submitted = parse(notification("sstack_submitted", Value::from("hi")));
        assert!(matches!(submitted, Inbound::Submitted(text) if text == "hi"));
        let failed = parse(notification("nvim_error_event", Value::from("bad")));
        assert!(matches!(failed, Inbound::Error(text) if text.contains("bad")));
    }

    #[test]
    fn messages_that_mean_nothing_here_are_passed_over() {
        let reply = to_value((1, 7, Value::Nil, Value::Nil)).unwrap();
        for message in [
            reply,
            notification("something_else", Value::Nil),
            notification("sstack_state", Value::from(1)),
            notification("sstack_submitted", Value::from(1)),
            to_value((NOTIFICATION, "sstack_state", Vec::<Value>::new())).unwrap(),
        ] {
            assert!(matches!(parse(message), Inbound::Other));
        }
    }
}
