//! Connections: the Providers that serve models, the credentials each Connection uses, the user
//! settings that name them, and moving a Session from one Connection to another between Turns.

mod config;
mod credentials;
mod provider;
mod switching;

pub use config::{Config, ConfigError, Connection, ProviderKind};
pub use credentials::{MissingSecret, SecretStore, SystemKeyring, resolve_secret};
pub use provider::{
    Effort, ModelRequest, Provider, ProviderError, StreamEvent, ToolSpec, scripted,
};
#[cfg(test)]
pub(crate) use switching::test_support;
pub use switching::{
    ConnectionError, Connections, ModelService, Opened, Switch, open_connection, record_connection,
};
