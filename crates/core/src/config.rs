//! User settings, chiefly the [`Connection`]s a run can use.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::provider::anthropic::AnthropicProvider;
use crate::provider::{Effort, Provider, ProviderError};

/// The name of the built-in [`Connection`].
const DEFAULT_CONNECTION: &str = "anthropic";
/// The model used unless settings or arguments choose another.
const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// The model that names Sessions unless settings choose another.
const DEFAULT_NAME_MODEL: &str = "claude-haiku-5-5";
/// The secret the built-in [`Connection`] authenticates with.
const DEFAULT_SECRET: &str = "ANTHROPIC_API_KEY";
/// The context window assumed for a [`Connection`] that does not set one, in tokens.
const DEFAULT_CONTEXT_LIMIT: u64 = 1_000_000;

/// The kind of Provider a [`Connection`] talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// The Claude API.
    Anthropic,
}

/// Supplies the context limit for settings that leave it out.
fn default_context_limit() -> u64 {
    DEFAULT_CONTEXT_LIMIT
}

/// Everything needed to open a connection to a given Provider.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// The kind of Provider to connect to.
    pub provider: ProviderKind,
    /// The name of the secret to look up, not the secret itself.
    pub secret: String,
    /// The API address to use instead of the Provider's default.
    pub base_url: Option<String>,
    /// How much effort the model should spend on each response.
    #[serde(default)]
    pub effort: Effort,
    /// The model's context window, in tokens.
    #[serde(default = "default_context_limit")]
    pub context_limit: u64,
    /// The model to request, which this Connection's Provider must serve. Missing means
    /// [`Config::default_model`].
    pub model: Option<String>,
    /// The model that writes Session Names on this Connection. Missing means
    /// [`Config::name_model`].
    pub name_model: Option<String>,
}

impl Connection {
    /// Opens this Connection with `secret`, ready to serve requests.
    ///
    /// # Errors
    /// Returns an error if the [`Provider`] cannot be set up.
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

/// What the user has chosen, which applies to every run unless it says otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The [`Connection`] used unless a run chooses another.
    pub default_connection: String,
    /// The model requested on a [`Connection`] that names none, unless a run chooses another.
    pub default_model: String,
    /// The [`Connection`]s available, by name.
    pub connections: BTreeMap<String, Connection>,
    /// Whether new Sessions edit their input with a real nvim, where one is installed.
    #[serde(default = "vim_by_default")]
    pub vim: bool,
    /// The model that sums up the first thing the person says in a Session as its Session Name,
    /// on a [`Connection`] that names none. It is served by the Session's own Connection. A
    /// small, fast model is enough.
    #[serde(default = "name_model_by_default")]
    pub name_model: String,
}

/// Returns the [`Config::name_model`] of settings that do not mention it.
fn name_model_by_default() -> String {
    DEFAULT_NAME_MODEL.to_owned()
}

/// Returns the [`Config::vim`] of settings that do not mention it.
fn vim_by_default() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        let anthropic = Connection {
            provider: ProviderKind::Anthropic,
            secret: DEFAULT_SECRET.to_owned(),
            base_url: None,
            effort: Effort::High,
            context_limit: DEFAULT_CONTEXT_LIMIT,
            model: None,
            name_model: None,
        };
        Self {
            default_connection: DEFAULT_CONNECTION.to_owned(),
            default_model: DEFAULT_MODEL.to_owned(),
            connections: BTreeMap::from([(DEFAULT_CONNECTION.to_owned(), anthropic)]),
            vim: vim_by_default(),
            name_model: name_model_by_default(),
        }
    }
}

/// A problem with the user's settings.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The settings file exists but could not be read.
    #[error("cannot read settings: {0}")]
    Read(#[from] io::Error),
    /// The settings file is not valid.
    #[error("invalid settings: {0}")]
    Invalid(#[from] toml::de::Error),
    /// A run asked for a [`Connection`] the settings do not define.
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

    /// Finds a [`Connection`] by name, or the default one if `name` is `None`.
    ///
    /// # Errors
    /// Returns an error if no Connection has that name.
    pub fn connection(&self, name: Option<&str>) -> Result<&Connection, ConfigError> {
        let name = name.unwrap_or(&self.default_connection);
        self.connections
            .get(name)
            .ok_or_else(|| ConfigError::UnknownConnection(name.to_owned()))
    }

    /// Returns the model to request on `connection`, unless a run chooses another.
    #[must_use]
    pub fn model<'a>(&'a self, connection: &'a Connection) -> &'a str {
        connection.model.as_deref().unwrap_or(&self.default_model)
    }

    /// Returns the model that writes Session Names on `connection`.
    #[must_use]
    pub fn name_model<'a>(&'a self, connection: &'a Connection) -> &'a str {
        connection.name_model.as_deref().unwrap_or(&self.name_model)
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
    fn vim_input_is_on_unless_settings_turn_it_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut settings = "default_connection = \"a\"\ndefault_model = \"b\"\n".to_owned();
        settings.push_str("connections = {}\n");
        fs::write(&path, format!("vim = false\n{settings}")).unwrap();
        assert!(!Config::load(&path).unwrap().vim);
        fs::write(&path, settings).unwrap();
        assert!(Config::load(&path).unwrap().vim);
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
            model = "home-model"
            name_model = "home-namer"
        "#;
        fs::write(&path, settings).unwrap();

        let config = Config::load(&path).unwrap();

        let work = config.connection(None).unwrap();
        assert_eq!(work.effort, Effort::Low);
        assert_eq!(config.model(work), "claude-sonnet-5-5");
        assert_eq!(config.name_model(work), DEFAULT_NAME_MODEL);
        let home = config.connection(Some("home")).unwrap();
        assert_eq!(home.secret, "HOME_KEY");
        assert_eq!(home.effort, Effort::High);
        assert_eq!(home.context_limit, DEFAULT_CONTEXT_LIMIT);
        assert_eq!(config.model(home), "home-model");
        assert_eq!(config.name_model(home), "home-namer");
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
