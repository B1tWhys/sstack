//! The UI-free core of sstack: Sessions, Providers, tools and the agent loop.
//!
//! The terminal front end is in [`sstack_cli`](../sstack_cli/index.html).

pub mod agent;
pub mod config;
pub mod credentials;
pub mod provider;
pub mod session;
pub mod skills;
pub mod tools;
pub mod vim;
pub mod workspace;
pub mod worktree;
