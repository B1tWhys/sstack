//! The Connections a Session can run on, and moving it from one to another between Turns.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bon::bon;

use crate::connection::{
    Config, ConfigError, Connection, Effort, MissingSecret, Provider, ProviderError, SecretStore,
    resolve_secret,
};
use crate::session::SessionMetadata;

/// Why a Session could not be put on a Connection.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    /// The Session Metadata recording the Connection could not be written.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The user's settings name no such Connection.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The Connection's secret is in none of the places it is looked for.
    #[error(transparent)]
    MissingSecret(#[from] MissingSecret),
    /// The Connection's Provider could not be set up with its secret.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// The model a Session moves to when the person switches its Connection, and what serves it.
pub struct ModelService {
    /// The service that runs the model.
    pub provider: Arc<dyn Provider>,
    /// The identifier of the model to request.
    pub model: String,
    /// How much effort the model is asked to spend, which the model can learn about the Session.
    pub effort: Effort,
    /// The model's context window, in tokens.
    pub context_limit: u64,
}

/// The Connections one Session can switch between, with the one it runs on now.
pub struct Connections {
    /// The settings the Session was opened with, which name every Connection.
    config: Config,
    /// The name of the Connection the Session runs on now.
    current: String,
    /// Where the Session is stored, beside which its choice of Connection is kept.
    log: PathBuf,
    /// The main checkout, where a `.env` file holding a secret would be.
    main: PathBuf,
    /// Where secrets are looked up before the environment and the `.env` file.
    keyring: Arc<dyn SecretStore + Send + Sync>,
}

/// Everything a Session runs on once it has moved to another Connection.
pub struct Switch {
    /// What serves the agent's model on that Connection.
    pub service: ModelService,
    /// What serves the model that writes Session Names on that Connection.
    pub namer: Arc<dyn Provider>,
    /// The model that writes Session Names on that Connection.
    pub name_model: String,
}

/// The services a Connection opens, one for the agent and one for naming the Session.
pub struct Opened {
    /// The service for the agent's model, asked for the Connection's effort.
    pub provider: Arc<dyn Provider>,
    /// The service for the model that names the Session, asked for little effort, since a name
    /// needs little thought.
    pub namer: Arc<dyn Provider>,
}

/// Opens `connection` with `secret`.
///
/// # Errors
/// Returns an error if the Connection's Provider cannot be set up.
pub fn open_connection(connection: &Connection, secret: String) -> Result<Opened, ConnectionError> {
    let namer = Connection {
        effort: Effort::Low,
        ..connection.clone()
    };
    Ok(Opened {
        namer: namer.provider(secret.clone())?,
        provider: connection.provider(secret)?,
    })
}

#[bon]
impl Connections {
    /// Returns the Connections in `config` for a Session, which runs on the one called
    /// `current`.
    #[builder]
    pub fn new(
        /// The settings that name every Connection.
        config: Config,
        /// The name of the Connection the Session runs on now.
        current: String,
        /// Where the Session is stored.
        log: PathBuf,
        /// The project's main checkout, where a `.env` file holding a secret would be.
        main: PathBuf,
        /// Where secrets are looked up before the environment and the `.env` file.
        keyring: Arc<dyn SecretStore + Send + Sync>,
    ) -> Self {
        Self {
            config,
            current,
            log,
            main,
            keyring,
        }
    }
}

impl Connections {
    /// Returns the name of the Connection the Session runs on now.
    #[must_use]
    pub fn current(&self) -> &str {
        &self.current
    }

    /// Returns the names of every Connection the Session can switch to, its own included, in
    /// order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.config.connections.keys().cloned().collect()
    }

    /// Opens the Connection called `name` for the Session, and records that the Session runs on
    /// it from now on, also after it is next opened. Returns nothing if the Session already runs
    /// on it.
    ///
    /// # Errors
    /// Returns an error, and changes nothing, if settings have no such Connection, its secret is
    /// in none of the places it is looked for, its Provider cannot be set up, or the choice
    /// cannot be recorded.
    pub fn switch(&mut self, name: &str) -> Result<Option<Switch>, ConnectionError> {
        if name == self.current {
            return Ok(None);
        }
        let connection = self.config.connection(Some(name))?;
        let secret = resolve_secret(&connection.secret, self.keyring.as_ref(), &self.main)?;
        let opened = open_connection(connection, secret)?;
        let switch = Switch {
            service: ModelService {
                provider: opened.provider,
                model: self.config.model(connection).to_owned(),
                effort: connection.effort,
                context_limit: connection.context_limit,
            },
            namer: opened.namer,
            name_model: self.config.name_model(connection).to_owned(),
        };
        record_connection(&self.log, name)?;
        name.clone_into(&mut self.current);
        Ok(Some(switch))
    }
}

/// Records in the Session Metadata of the Session at `log` that it runs on the Connection called
/// `name`.
///
/// # Errors
/// Returns an error if the Session Metadata cannot be changed.
pub fn record_connection(log: &Path, name: &str) -> Result<(), ConnectionError> {
    SessionMetadata::update(log, |metadata| metadata.connection = Some(name.to_owned()))?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Connections that need no real secrets.

    use std::collections::HashMap;

    use super::*;

    /// A keyring holding the given secrets and nothing else.
    #[derive(Default)]
    pub struct Keyring(pub HashMap<String, String>);

    impl SecretStore for Keyring {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    /// Returns the built-in Connections for the Session at `log`, running on `anthropic`, with
    /// a keyring that has only the secret of `openai`.
    pub fn built_in(log: PathBuf) -> Connections {
        let main = log.parent().unwrap().to_owned();
        let keyring = Keyring(HashMap::from([(
            "OPENAI_API_KEY".to_owned(),
            "key".to_owned(),
        )]));
        Connections::builder()
            .config(Config::default())
            .current("anthropic".into())
            .log(log)
            .main(main)
            .keyring(Arc::new(keyring))
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::built_in;
    use super::*;
    use crate::connection::ConfigError;

    #[test]
    fn a_switch_opens_the_connection_with_its_own_models_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("1.jsonl");
        let mut connections = built_in(log.clone());
        assert_eq!(connections.names(), ["anthropic", "deepseek", "openai"]);

        let switch = connections.switch("openai").unwrap().unwrap();

        assert_eq!(switch.service.model, "gpt-6.1-sol");
        assert_eq!(switch.service.effort, Effort::High);
        assert_eq!(switch.name_model, "gpt-6-luna");
        assert_eq!(connections.current(), "openai");
        let kept = SessionMetadata::read(&log).unwrap().connection;
        assert_eq!(kept.as_deref(), Some("openai"));
        assert!(connections.switch("openai").unwrap().is_none());
    }

    #[test]
    fn a_connection_that_cannot_be_opened_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("1.jsonl");
        let mut connections = built_in(log.clone());
        let unset = Connection {
            secret: "SSTACK_TEST_SECRET_NOTHING_SETS".into(),
            ..connections.config.connection(None).unwrap().clone()
        };
        connections.config.connections.insert("unset".into(), unset);

        let missing = connections.switch("unset").err().unwrap();
        let unknown = connections.switch("nope").err().unwrap();

        assert!(
            matches!(missing, ConnectionError::MissingSecret(_)),
            "{missing}"
        );
        assert!(matches!(
            unknown,
            ConnectionError::Config(ConfigError::UnknownConnection(_))
        ));
        assert_eq!(connections.current(), "anthropic");
        assert_eq!(SessionMetadata::read(&log).unwrap().connection, None);
    }
}
