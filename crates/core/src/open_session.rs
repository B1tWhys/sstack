//! Open Sessions: setting a Session up to run Turns, and what an interface keeps for each one it
//! has open, so that every interface follows the same rules for what the person's input does.

mod controller;
mod reporting;
mod setup;
mod state;

pub use controller::{NoOtherSessions, OpenSessions, SessionController};
pub use reporting::{ReportingApprover, ReportingAsker};
pub use setup::{
    AskedConnection, LATEST, Place, Run, SetupError, Start, installed_skills, listed_sessions,
    main_checkout, prepare, sessions_directory, starting_session,
};
pub use state::{Exit, Outcome, Refusal, SessionEvent, SessionState};
