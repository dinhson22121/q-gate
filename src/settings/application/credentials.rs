//! Credential port and the use cases that read and store secrets.

use std::io;
use std::path::Path;

use crate::settings::domain::credentials::{CredentialName, Secret};

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("invalid secret: {0}")]
    InvalidSecret(&'static str),
    /// `<NAME>_FILE` points to a file that cannot be used. Never contains the file content.
    #[error("cannot read secret file {path} (from {var}): {reason}")]
    SecretFile { var: &'static str, path: String, reason: String },
    #[error("credential store error: {0}")]
    Store(String),
}

/// Persistent secret storage (OS keychain, credentials file...).
pub trait CredentialStore: Send + Sync {
    fn get(&self, name: CredentialName) -> Result<Option<Secret>, CredentialError>;
    fn set(&self, name: CredentialName, secret: &Secret) -> Result<(), CredentialError>;
    /// Removing a secret that is not stored is not an error.
    fn delete(&self, name: CredentialName) -> Result<(), CredentialError>;
}

/// Finds a secret: `<NAME>` env var, then the file named by `<NAME>_FILE`, then `store`.
///
/// Empty or whitespace-only env values count as unset. A `<NAME>_FILE` that cannot be read
/// or is empty is an error rather than a silent fall-through, since it is an explicit setting.
pub fn resolve<S>(
    name: CredentialName,
    env_lookup: impl Fn(&str) -> Option<String>,
    read_file: impl Fn(&Path) -> io::Result<String>,
    store: &S,
) -> Result<Option<Secret>, CredentialError>
where
    S: CredentialStore + ?Sized,
{
    let non_empty = |var: &str| env_lookup(var).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    if let Some(value) = non_empty(name.env_var()) {
        return Ok(Some(Secret::new(value)));
    }
    if let Some(path) = non_empty(name.file_env_var()) {
        return read_secret_file(name.file_env_var(), &path, read_file).map(Some);
    }
    store.get(name)
}

fn read_secret_file(
    var: &'static str,
    path: &str,
    read_file: impl Fn(&Path) -> io::Result<String>,
) -> Result<Secret, CredentialError> {
    let error = |reason: String| CredentialError::SecretFile { var, path: path.to_string(), reason };
    let content = read_file(Path::new(path)).map_err(|e| error(e.kind().to_string()))?;
    let value = content.trim();
    if value.is_empty() {
        return Err(error("file is empty".into()));
    }
    Ok(Secret::new(value))
}

/// Validates and stores a secret typed or pasted by the user. Surrounding whitespace is trimmed.
pub fn store_secret<S>(name: CredentialName, raw: &str, store: &S) -> Result<(), CredentialError>
where
    S: CredentialStore + ?Sized,
{
    let value = raw.trim();
    if value.is_empty() {
        return Err(CredentialError::InvalidSecret("value is empty"));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(CredentialError::InvalidSecret("value must not contain whitespace"));
    }
    store.set(name, &Secret::new(value))
}

/// Removes a stored secret (no-op when absent).
pub fn delete_secret<S>(name: CredentialName, store: &S) -> Result<(), CredentialError>
where
    S: CredentialStore + ?Sized,
{
    store.delete(name)
}

#[cfg(test)]
pub(crate) mod fakes {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// In-memory store; `failing` makes every call return a store error.
    #[derive(Default)]
    pub struct MemoryStore {
        pub values: Mutex<HashMap<CredentialName, String>>,
        pub failing: bool,
    }

    impl MemoryStore {
        pub fn failing() -> Self {
            Self { failing: true, ..Self::default() }
        }

        fn check(&self) -> Result<(), CredentialError> {
            if self.failing { Err(CredentialError::Store("unavailable".into())) } else { Ok(()) }
        }
    }

    impl CredentialStore for MemoryStore {
        fn get(&self, name: CredentialName) -> Result<Option<Secret>, CredentialError> {
            self.check()?;
            Ok(self.values.lock().unwrap().get(&name).map(Secret::new))
        }

        fn set(&self, name: CredentialName, secret: &Secret) -> Result<(), CredentialError> {
            self.check()?;
            self.values.lock().unwrap().insert(name, secret.expose().to_string());
            Ok(())
        }

        fn delete(&self, name: CredentialName) -> Result<(), CredentialError> {
            self.check()?;
            self.values.lock().unwrap().remove(&name);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::fakes::MemoryStore;
    use super::*;

    const KEY: CredentialName = CredentialName::AnthropicApiKey;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| map.get(name).cloned()
    }

    fn no_files(_: &Path) -> io::Result<String> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }

    fn store_with(value: &str) -> MemoryStore {
        let store = MemoryStore::default();
        store_secret(KEY, value, &store).unwrap();
        store
    }

    #[test]
    fn env_var_wins_over_file_and_store() {
        let store = store_with("from-store");
        let found = resolve(
            KEY,
            env(&[("ANTHROPIC_API_KEY", " from-env "), ("ANTHROPIC_API_KEY_FILE", "/x")]),
            no_files,
            &store,
        );
        assert_eq!(found.unwrap().unwrap().expose(), "from-env");
    }

    #[test]
    fn secret_file_is_used_when_env_var_is_empty() {
        let store = store_with("from-store");
        let read = |path: &Path| {
            assert_eq!(path, Path::new("/run/secrets/key"));
            Ok("from-file\n".to_string())
        };
        let found = resolve(
            KEY,
            env(&[("ANTHROPIC_API_KEY", ""), ("ANTHROPIC_API_KEY_FILE", "/run/secrets/key")]),
            read,
            &store,
        );
        assert_eq!(found.unwrap().unwrap().expose(), "from-file");
    }

    #[test]
    fn unreadable_or_empty_secret_file_is_an_error_without_content() {
        let store = store_with("from-store");
        let err = resolve(KEY, env(&[("ANTHROPIC_API_KEY_FILE", "/missing")]), no_files, &store).unwrap_err();
        assert!(matches!(err, CredentialError::SecretFile { var: "ANTHROPIC_API_KEY_FILE", .. }));
        assert!(err.to_string().contains("/missing"));

        let empty = |_: &Path| Ok("  \n".to_string());
        let err = resolve(KEY, env(&[("ANTHROPIC_API_KEY_FILE", "/empty")]), empty, &store).unwrap_err();
        assert!(err.to_string().contains("file is empty"));
    }

    #[test]
    fn falls_back_to_store() {
        let store = store_with("from-store");
        let found = resolve(KEY, env(&[]), no_files, &store).unwrap();
        assert_eq!(found.unwrap().expose(), "from-store");
        assert!(resolve(CredentialName::JiraApiToken, env(&[]), no_files, &store).unwrap().is_none());
    }

    #[test]
    fn store_errors_propagate() {
        let err = resolve(KEY, env(&[]), no_files, &MemoryStore::failing()).unwrap_err();
        assert!(matches!(err, CredentialError::Store(_)));
    }

    #[test]
    fn store_secret_trims_and_rejects_invalid_values() {
        let store = store_with("  sk-ant-123 \n");
        assert_eq!(store.get(KEY).unwrap().unwrap().expose(), "sk-ant-123");
        assert!(matches!(store_secret(KEY, "  ", &store), Err(CredentialError::InvalidSecret(_))));
        assert!(matches!(store_secret(KEY, "sk ant", &store), Err(CredentialError::InvalidSecret(_))));
    }

    #[test]
    fn delete_secret_removes_value() {
        let store = store_with("v");
        delete_secret(KEY, &store).unwrap();
        assert!(store.get(KEY).unwrap().is_none());
    }
}
