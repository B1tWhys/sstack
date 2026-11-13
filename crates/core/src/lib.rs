//! The UI-free core of sstack: Sessions, Providers, the tool port and the agent loop.
//!
//! The terminal front end is in [`sstack_cli`](../sstack_cli/index.html), the windowed one in
//! [`sstack_gui`](../sstack_gui/index.html), and the tools in
//! [`sstack_tools`](../sstack_tools/index.html). Each depends on this crate, never the reverse.
//!
//! The modules import only downwards through these layers:
//!
//! 1. [`running_harness`]
//! 2. [`open_session`]
//! 3. [`agent`]
//! 4. [`tools`], the port the agent calls tools through
//! 5. [`session`], [`connection`], [`commands`], [`workspace`] and [`vim`], which may import
//!    each other
//! 6. [`protocol`], which imports nothing else in core
//!
//! `docs/adr/0016-core-is-the-inside-and-everything-else-depends-on-it.md` says why.

pub mod agent;
pub mod commands;
pub mod connection;
pub mod open_session;
pub mod protocol;
pub mod running_harness;
pub mod session;
pub mod tools;
pub mod vim;
pub mod workspace;
