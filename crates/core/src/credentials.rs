//! Where the secret a [`Connection`](crate::config::Connection) names comes from.

use std::path::Path;
use std::sync::Arc;

use keyring_core::{CredentialStore, Entry, set_default_store};

/// The service name secrets are filed under in the system keyring.
const KEYRING_SERVICE: &str = "sstack";

/// No source held the named secret.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("no value for {0}: add it to the system keychain, the environment, or a .env file")]
pub struct MissingSecret(pub String);

/// A secure place to keep secrets, as an alternative to the environment and `.env` files.
pub trait SecretStore {
    /// Returns the secret called `name`, if this store has it.
    fn get(&self, name: &str) -> Option<String>;
}

/// Secret storage in the operating system's credential store.
#[derive(Debug, Default)]
pub struct SystemKeyring;

impl SystemKeyring {
    /// Makes the store available to this process. Failure is not an error, because a machine
    /// without one can still use the other sources.
    pub fn connect() {
        if let Ok(store) = platform_store() {
            set_default_store(store);
        }
    }
}

impl SecretStore for SystemKeyring {
    fn get(&self, name: &str) -> Option<String> {
        Entry::new(KEYRING_SERVICE, name).ok()?.get_password().ok()
    }
}

/// Opens the credential store of the operating system this was built for.
#[cfg(target_os = "macos")]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(target_os = "linux")]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

/// Looks up `name` in `keyring`, then the environment, then the `.env` file in `directory`.
///
/// # Errors
/// Returns an error naming the secret if no source has it.
pub fn resolve_secret(
    name: &str,
    keyring: &dyn SecretStore,
    directory: &Path,
) -> Result<String, MissingSecret> {
    keyring
        .get(name)
        .or_else(|| non_empty(std::env::var(name).ok()))
        .or_else(|| dotenv_value(directory, name))
        .ok_or_else(|| MissingSecret(name.to_owned()))
}

/// Treats an empty value the same as no value.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// Reads `name` from the `.env` file in `directory`, if the file and the value exist. Lines that
/// cannot be parsed are skipped.
///
/// Values follow dotenv rules: `$` expands from the environment unless the value is in single
/// quotes, so a secret containing one needs them.
fn dotenv_value(directory: &Path, name: &str) -> Option<String> {
    dotenvy::from_path_iter(directory.join(".env"))
        .ok()?
        // A failed read fails again on every retry, so skipping it like a bad line would hang.
        .take_while(|assignment| !matches!(assignment, Err(dotenvy::Error::Io(_))))
        .flatten()
        .find_map(|(key, value)| non_empty((key == name).then_some(value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Stored(Option<&'static str>);

    impl SecretStore for Stored {
        fn get(&self, _name: &str) -> Option<String> {
            self.0.map(str::to_owned)
        }
    }

    #[test]
    fn the_keyring_wins_over_a_dotenv_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "SSTACK_TEST_A=from-file\n").unwrap();
        let found = resolve_secret("SSTACK_TEST_A", &Stored(Some("stored")), dir.path());
        assert_eq!(found.unwrap(), "stored");
    }

    #[test]
    fn the_environment_wins_over_a_dotenv_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "HOME=from-file\n").unwrap();
        let found = resolve_secret("HOME", &Stored(None), dir.path()).unwrap();
        assert_eq!(found, std::env::var("HOME").unwrap());
    }

    #[test]
    fn dotenv_files_supply_only_the_requested_value() {
        let dir = tempfile::tempdir().unwrap();
        let contents = "# comment\nOTHER=no\nnot a pair\nexport SSTACK_TEST_B=\"quoted\"\n";
        fs::write(dir.path().join(".env"), contents).unwrap();
        let found = resolve_secret("SSTACK_TEST_B", &Stored(None), dir.path());
        assert_eq!(found.unwrap(), "quoted");
    }

    #[test]
    fn an_empty_value_counts_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "SSTACK_TEST_C=\n").unwrap();
        let found = resolve_secret("SSTACK_TEST_C", &Stored(None), dir.path());
        assert_eq!(found.unwrap_err(), MissingSecret("SSTACK_TEST_C".into()));
    }

    #[test]
    fn single_quotes_keep_a_dollar_sign_in_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "SSTACK_TEST_E='pa$$word'\n").unwrap();
        let found = resolve_secret("SSTACK_TEST_E", &Stored(None), dir.path());
        assert_eq!(found.unwrap(), "pa$$word");
    }

    #[test]
    fn a_directory_in_place_of_a_dotenv_file_counts_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".env")).unwrap();
        let found = resolve_secret("SSTACK_TEST_F", &Stored(None), dir.path());
        assert_eq!(found.unwrap_err(), MissingSecret("SSTACK_TEST_F".into()));
    }

    #[test]
    fn a_missing_secret_names_itself_without_a_dotenv_file() {
        let dir = tempfile::tempdir().unwrap();
        let error = resolve_secret("SSTACK_TEST_D", &Stored(None), dir.path()).unwrap_err();
        assert!(error.to_string().contains("SSTACK_TEST_D"));
    }

    #[test]
    fn the_system_keyring_reports_an_absent_secret_as_none() {
        set_default_store(keyring_core::mock::Store::new().unwrap());
        assert_eq!(SystemKeyring.get("name"), None);
    }
}
