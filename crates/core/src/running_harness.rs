//! The Running Harness as core sees it: what each of its Sessions is doing, what quitting or a
//! Rebuild does to all of them, the order interfaces list them in, and the Rebuild itself.

mod queue;
mod rebuild;
mod rules;

pub use queue::Waits;
pub use rebuild::{Binary, build_harness, relaunch_into, source_to_build};
pub use rules::{
    Effect, Exit, InSession, Interrupt, Outcome, Phase, Refusal, RunningHarness, SessionEvent,
    SessionState, Trigger, interrupt,
};
