//! `LoadEffectiveSettings`: the server settings in effect at startup.

use std::sync::Arc;

use super::ports::{EnvLookup, SeedConfigSource, ServerConfigStore, SettingsError};
use crate::settings::domain::ServerConfig;

/// `changed_by` of the first version when there was no `qgate.toml` to import.
pub const DEFAULTS_ORIGIN: &str = "init:defaults";

/// Precedence: env overrides > latest saved version > (first start only) the imported `qgate.toml` > defaults.
///
/// On the first start the seed (or the defaults) is saved as version 1, so `qgate.toml` is never read again.
pub struct LoadEffectiveSettings {
    store: Arc<dyn ServerConfigStore>,
    seed: Arc<dyn SeedConfigSource>,
    env: EnvLookup,
}

impl LoadEffectiveSettings {
    pub fn new(store: Arc<dyn ServerConfigStore>, seed: Arc<dyn SeedConfigSource>, env: EnvLookup) -> Self {
        Self { store, seed, env }
    }

    pub fn execute(&self) -> Result<ServerConfig, SettingsError> {
        let raw = match self.store.load_raw()? {
            Some(raw) => raw,
            None => self.seed_store()?,
        };
        let effective = raw.with_env_overrides(|name| (self.env)(name));
        effective.validate().map_err(SettingsError::Invalid)?;
        Ok(effective)
    }

    fn seed_store(&self) -> Result<ServerConfig, SettingsError> {
        let (config, origin) = match self.seed.load()? {
            Some(config) => (config, self.seed.origin().to_string()),
            None => (ServerConfig::default(), DEFAULTS_ORIGIN.to_string()),
        };
        // Refuse to store an invalid file; report it against the file so the user can fix it.
        config
            .clone()
            .with_env_overrides(|name| (self.env)(name))
            .validate()
            .map_err(|e| SettingsError::Storage(format!("invalid settings in {origin}: {e}")))?;
        self.store.seed(&config, &origin)?;
        // Another process may have seeded first: its version wins.
        Ok(self.store.load_raw()?.unwrap_or(config))
    }
}
