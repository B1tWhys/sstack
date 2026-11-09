//! The UI-free core of sstack: Sessions, Providers, tools and the agent loop.
//!
//! The terminal front end is in [`sstack_cli`](../sstack_cli/index.html), and the windowed one
//! in [`sstack_gui`](../sstack_gui/index.html).

pub mod agent;
pub mod config;
pub mod credentials;
pub mod human_command;
pub mod provider;
pub mod question;
pub mod queue;
pub mod rebuild;
pub mod running_harness;
pub mod sandbox;
pub mod session;
pub mod session_controller;
pub mod session_name;
pub mod session_report;
pub mod session_setup;
pub mod skills;
pub mod tools;
pub mod turn_runner;
pub mod vim;
pub mod workspace;
pub mod worktree;
