//! The parts of the interactive screen. Each one owns its state and reacts to events alone.

mod approval;
mod prompt;
mod sidebar;
mod status;
mod transcript;

pub use approval::Approval;
pub use prompt::Prompt;
pub use sidebar::{Activity, Row, Sidebar, WIDTH as SIDEBAR_WIDTH};
pub use status::Status;
pub use transcript::Transcript;
