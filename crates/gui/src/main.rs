//! The `sstack-gui` command.

use std::process::ExitCode;

/// Hands the process over to the library.
fn main() -> ExitCode {
    sstack_gui::run_from_command_line()
}
