//! User settings, chiefly the Connections a run can use.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::provider::anthropic::AnthropicProvider;
use crate::provider::{Effort, Provider, ProviderError};

const DEFAULT_CONNECTION: &str = "anthropic";
const DEFAULT_MODEL: &str = "claude-opus-5-5";
const DEFAULT_SECRET: &str = "ANTHROPIC_API_KEY";
const DEFAULT_CONTEXT_LIMIT: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
}

fn default_context_limit() -> u64 {
    DEFAULT_CONTEXT_LIMIT
}

/// A Provider paired with the name of the secret that authenticates it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub provider: ProviderKind,
    pub secret: String,
    pub base_url: Option<String>,
    #[serde(default)]
    pub effort: Effort,
    #[serde(default = "default_context_limit")]
    pub context_limit: u64,
}

impl Connection {
    /// # Errors
    /// Returns an error if the Provider's client cannot be built.
    pub fn provider(&self, secret: String) -> Result<Arc<dyn Provider>, ProviderError> {
        match self.provider {
            ProviderKind::Anthropic => Ok(Arc::new(AnthropicProvider::new(
                secret,
                self.base_url.clone(),
                self.effort,
            )?)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub default_connection: String,
    pub default_model: String,
    pub connections: BTreeMap<String, Connection>,
}

impl Default for Config {
    fn default() -> Self {
        let anthropic = Connection {
            provider: ProviderKind::Anthropic,
            secret: DEFAULT_SECRET.to_owned(),
            base_url: None,
            effort: Effort::High,
            context_limit: DEFAULT_CONTEXT_LIMIT,
        };
        Self {
            default_connection: DEFAULT_CONNECTION.to_owned(),
            default_model: DEFAULT_MODEL.to_owned(),
            connections: BTreeMap::from([(DEFAULT_CONNECTION.to_owned(), anthropic)]),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read settings: {0}")]
    Read(#[from] io::Error),
    #[error("invalid settings: {0}")]
    Invalid(#[from] toml::de::Error),
    #[error("no connection named {0}")]
    UnknownConnection(String),
}

impl Config {
    /// Reads settings from `path`, using the defaults if the file does not exist.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(contents) => Ok(toml::from_str(&contents)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Finds a Connection by name, or the default one if `name` is `None`.
    ///
    /// # Errors
    /// Returns an error if no Connection has that name.
    pub fn connection(&self, name: Option<&str>) -> Result<&Connection, ConfigError> {
        let name = name.unwrap_or(&self.default_connection);
        self.connections
            .get(name)
            .ok_or_else(|| ConfigError::UnknownConnection(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.connection(None).unwrap().secret, DEFAULT_SECRET);
    }

    #[test]
    fn reads_connections_and_selects_them_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let settings = r#"
            default_connection = "work"
            default_model = "claude-sonnet-5-5"

            [connections.work]
            provider = "anthropic"
            secret = "WORK_KEY"
            effort = "low"

            [connections.home]
            provider = "anthropic"
            secret = "HOME_KEY"
            base_url = "http://localhost:1/"
        "#;
        fs::write(&path, settings).unwrap();

        let config = Config::load(&path).unwrap();

        assert_eq!(config.connection(None).unwrap().effort, Effort::Low);
        let home = config.connection(Some("home")).unwrap();
        assert_eq!(home.secret, "HOME_KEY");
        assert_eq!(home.effort, Effort::High);
        assert_eq!(home.context_limit, DEFAULT_CONTEXT_LIMIT);
        assert!(home.provider("key".into()).is_ok());
    }

    #[test]
    fn invalid_settings_and_unknown_connections_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "nonsense = true").unwrap();
        assert!(matches!(Config::load(&path), Err(ConfigError::Invalid(_))));
        assert!(matches!(
            Config::load(dir.path()),
            Err(ConfigError::Read(_))
        ));
        assert!(matches!(
            Config::default().connection(Some("nope")),
            Err(ConfigError::UnknownConnection(_))
        ));
    }
}
