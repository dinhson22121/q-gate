//! Server settings use cases: load at startup, view/update/rollback from the settings page, and secrets.

mod fields;
mod load;
mod ports;
mod secrets;
mod use_cases;

#[cfg(test)]
pub(crate) mod fakes;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub use load::{DEFAULTS_ORIGIN, LoadEffectiveSettings};
pub use ports::{EnvLookup, SeedConfigSource, ServerConfigStore, SettingsError, StoredSettings};
pub use secrets::{ClearSecret, SecretSource, SecretStatus, SetSecret};
pub use use_cases::{
    Applied, GetSettings, HistoryEntry, LockedField, PAGE_CHANGED_BY, RollbackSettings, SettingsHistory, SettingsView,
    UpdateSettings, WebhookChange, latest_version,
};

use super::SharedServerConfig;
use super::credentials::CredentialStore;
use crate::settings::domain::ServerConfig;

/// Everything the settings-page use cases need. Build once per process and share it (`Arc`).
pub struct SettingsDeps {
    store: Arc<dyn ServerConfigStore>,
    credentials: Arc<dyn CredentialStore>,
    shared: SharedServerConfig,
    env: EnvLookup,
    /// Effective config the process started with: restart-only fields keep these values until restart.
    boot: ServerConfig,
    /// Serializes read-modify-write sequences (two tabs saving at once).
    write_lock: Mutex<()>,
}

impl SettingsDeps {
    /// `shared` must already hold the effective config loaded at startup (it is taken as the boot snapshot).
    pub fn new(
        store: Arc<dyn ServerConfigStore>,
        credentials: Arc<dyn CredentialStore>,
        shared: SharedServerConfig,
        env: EnvLookup,
    ) -> Self {
        let boot = read_shared(&shared);
        Self { store, credentials, shared, env, boot, write_lock: Mutex::new(()) }
    }

    fn write_guard(&self) -> MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn effective(&self, raw: ServerConfig) -> ServerConfig {
        raw.with_env_overrides(|name| (self.env)(name))
    }

    fn replace_shared(&self, config: ServerConfig) {
        *self.shared.write().unwrap_or_else(PoisonError::into_inner) = config;
    }
}

fn read_shared(shared: &SharedServerConfig) -> ServerConfig {
    shared.read().unwrap_or_else(PoisonError::into_inner).clone()
}
