pub mod config_file;
pub mod fallback_store;
pub mod file_store;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod keyring_store;
pub mod paths;
pub mod seed_file;
pub mod sqlite_store;
pub mod templates;

pub use config_file::{
    CONFIG_FILE_NAME, ConfigError, load_config, load_server_config, parse_config, write_server_template, write_template,
};
pub use fallback_store::FallbackStore;
pub use file_store::FileStore;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use keyring_store::KeyringStore;
pub use paths::{AppPaths, AppPathsError};
pub use seed_file::{SEED_ORIGIN, TomlSeedSource, process_env};
pub use sqlite_store::SqliteServerConfigStore;
pub use templates::{CONFIG_TEMPLATE, SERVER_CONFIG_TEMPLATE};

use crate::settings::application::credentials::{self, CredentialError, CredentialStore};
use crate::settings::domain::credentials::{CredentialName, Secret};

pub fn default_store(paths: &AppPaths) -> Box<dyn CredentialStore> {
    let file = FileStore::new(paths.credentials_file());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Box::new(FallbackStore::new(KeyringStore::new(), file))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Box::new(file)
    }
}

pub fn resolve_credential(
    name: CredentialName,
    store: &dyn CredentialStore,
) -> Result<Option<Secret>, CredentialError> {
    credentials::resolve(name, |var| std::env::var(var).ok(), |path| std::fs::read_to_string(path), store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_credential_reads_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("credentials.toml"));
        assert!(resolve_credential(CredentialName::JiraApiToken, &store).is_ok());
    }

    #[test]
    fn default_store_is_constructible() {
        let dir = tempfile::tempdir().unwrap();
        let _store = default_store(&AppPaths::with_dirs(dir.path(), dir.path()));
    }
}
