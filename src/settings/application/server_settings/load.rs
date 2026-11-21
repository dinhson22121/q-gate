use std::sync::Arc;

use super::ports::{EnvLookup, SeedConfigSource, ServerConfigStore, SettingsError};
use crate::settings::domain::ServerConfig;

pub const DEFAULTS_ORIGIN: &str = "init:defaults";

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
        config
            .clone()
            .with_env_overrides(|name| (self.env)(name))
            .validate()
            .map_err(|e| SettingsError::Storage(format!("invalid settings in {origin}: {e}")))?;
        self.store.seed(&config, &origin)?;
        Ok(self.store.load_raw()?.unwrap_or(config))
    }
}
