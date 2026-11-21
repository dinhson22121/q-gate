use crate::settings::application::credentials::{CredentialError, CredentialStore};
use crate::settings::domain::credentials::{CredentialName, Secret};

pub const KEYRING_SERVICE: &str = "qgate";

#[derive(Debug, Clone, Default)]
pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(name: CredentialName) -> Result<keyring::Entry, CredentialError> {
        keyring::Entry::new(KEYRING_SERVICE, name.as_str()).map_err(store_error)
    }
}

impl CredentialStore for KeyringStore {
    fn get(&self, name: CredentialName) -> Result<Option<Secret>, CredentialError> {
        map_get(Self::entry(name)?.get_password())
    }

    fn set(&self, name: CredentialName, secret: &Secret) -> Result<(), CredentialError> {
        Self::entry(name)?.set_password(secret.expose()).map_err(store_error)
    }

    fn delete(&self, name: CredentialName) -> Result<(), CredentialError> {
        map_delete(Self::entry(name)?.delete_credential())
    }
}

fn map_get(result: keyring::Result<String>) -> Result<Option<Secret>, CredentialError> {
    match result {
        Ok(value) => Ok(Some(Secret::new(value))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(store_error(e)),
    }
}

fn map_delete(result: keyring::Result<()>) -> Result<(), CredentialError> {
    match result {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(store_error(e)),
    }
}

fn store_error(error: keyring::Error) -> CredentialError {
    CredentialError::Store(format!("OS keychain: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_entries_are_not_errors() {
        assert!(map_get(Err(keyring::Error::NoEntry)).unwrap().is_none());
        assert!(map_delete(Err(keyring::Error::NoEntry)).is_ok());
        assert!(map_delete(Ok(())).is_ok());
    }

    #[test]
    fn values_and_failures_are_mapped() {
        assert_eq!(map_get(Ok("s".into())).unwrap().unwrap().expose(), "s");
        let failure = || keyring::Error::NoStorageAccess("locked".into());
        assert!(matches!(map_get(Err(failure())), Err(CredentialError::Store(_))));
        assert!(matches!(map_delete(Err(failure())), Err(CredentialError::Store(_))));
    }
}
