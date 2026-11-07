//! The `sstack` command.

use std::process::ExitCode;

/// Hands the process over to the library.
fn main() -> ExitCode {
    sstack_cli::run_from_command_line()
}
