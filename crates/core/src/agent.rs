//! The agent: the loop that carries a Session forward one Turn at a time, running Turns in the
//! background, and the prompts that wait for it while a Turn runs.

mod queued_prompts;
mod replay;
mod runner;
mod turn;

pub use queued_prompts::QueuedPrompts;
pub use replay::replay;
pub use runner::AgentHandle;
pub use turn::{Agent, AgentConfig, AgentError, EventSink, ModelService, TurnContext};
