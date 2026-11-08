//! The terminal front end of sstack, run interactively or for a single prompt.
//!
//! Sessions, Providers, tools and the agent loop are in [`sstack`].

mod describe;
mod headless;
mod setup;
mod tui;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use sstack::rebuild::Binary;

/// A coding agent that improves itself.
#[derive(Debug, Parser)]
#[command(name = "sstack")]
struct Args {
    /// Run one prompt without interaction. Shell commands are denied.
    #[arg(short, long)]
    prompt: Option<String>,
    /// Connection to use, instead of the default in settings.
    #[arg(long)]
    connection: Option<String>,
    /// Model to use, instead of the default in settings.
    #[arg(long)]
    model: Option<String>,
    /// Resume a Session by id, or the newest one if no id is given.
    #[arg(long, num_args = 0..=1, default_missing_value = LATEST)]
    resume: Option<String>,
}

/// The `--resume` value that means the newest Session.
const LATEST: &str = "latest";

/// The binary a Rebuild builds and relaunches into, which is this one.
const HARNESS: Binary = Binary {
    package: env!("CARGO_PKG_NAME"),
    name: "sstack",
};

/// Runs the mode the arguments ask for.
async fn run(args: Args) -> Result<ExitCode> {
    match args.prompt.as_deref() {
        Some(prompt) => headless::run(&args, prompt).await,
        None => tui::run(&args).await,
    }
}

/// Runs sstack with the process's arguments and reports how it went.
///
/// # Panics
///
/// Panics if the async runtime cannot start.
#[tokio::main]
pub async fn run_from_command_line() -> ExitCode {
    report(run(Args::parse()).await)
}

/// Prints a failure and returns the matching exit code.
fn report(result: Result<ExitCode>) -> ExitCode {
    result.unwrap_or_else(|error| {
        eprintln!("error: {error}");
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    /// Parses `cli` as if it followed the program name on the command line.
    pub fn args(cli: &[&str]) -> Args {
        Args::parse_from(std::iter::once("sstack").chain(cli.iter().copied()))
    }

    #[test]
    fn resume_accepts_an_id_or_means_the_newest() {
        assert_eq!(args(&["--resume"]).resume.as_deref(), Some(LATEST));
        assert_eq!(args(&["--resume", "12"]).resume.as_deref(), Some("12"));
        assert_eq!(args(&[]).resume, None);
    }

    #[test]
    fn failures_exit_unsuccessfully() {
        let failure = format!("{:?}", ExitCode::FAILURE);
        assert_eq!(format!("{:?}", report(Err(anyhow!("boom")))), failure);
    }
}
