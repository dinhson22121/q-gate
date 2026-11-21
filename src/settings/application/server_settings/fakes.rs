//! In-memory fakes of the settings ports, for unit tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::ports::{EnvLookup, SeedConfigSource, ServerConfigStore, SettingsError, StoredSettings};
use crate::settings::domain::ServerConfig;

#[derive(Default)]
pub struct MemoryConfigStore {
    pub rows: Mutex<Vec<StoredSettings>>,
}

impl MemoryConfigStore {
    fn push(&self, config: &ServerConfig, changed_by: &str) -> i64 {
        let mut rows = self.rows.lock().unwrap();
        let version = rows.len() as i64 + 1;
        rows.push(StoredSettings {
            version,
            changed_by: changed_by.into(),
            created_at: format!("t{version}"),
            config: config.clone(),
        });
        version
    }
}

impl ServerConfigStore for MemoryConfigStore {
    fn load_raw(&self) -> Result<Option<ServerConfig>, SettingsError> {
        Ok(self.rows.lock().unwrap().last().map(|r| r.config.clone()))
    }

    fn save_raw(&self, config: &ServerConfig, changed_by: &str) -> Result<i64, SettingsError> {
        Ok(self.push(config, changed_by))
    }

    fn seed(&self, config: &ServerConfig, changed_by: &str) -> Result<Option<i64>, SettingsError> {
        if self.rows.lock().unwrap().is_empty() { Ok(Some(self.push(config, changed_by))) } else { Ok(None) }
    }

    fn version(&self, version: i64) -> Result<Option<StoredSettings>, SettingsError> {
        Ok(self.rows.lock().unwrap().iter().find(|r| r.version == version).cloned())
    }

    fn recent(&self, limit: u32) -> Result<Vec<StoredSettings>, SettingsError> {
        Ok(self.rows.lock().unwrap().iter().rev().take(limit as usize).cloned().collect())
    }
}

pub struct FixedSeed(pub Option<ServerConfig>);

impl SeedConfigSource for FixedSeed {
    fn origin(&self) -> &str {
        "import:qgate.toml"
    }

    fn load(&self) -> Result<Option<ServerConfig>, SettingsError> {
        Ok(self.0.clone())
    }
}

pub fn env(pairs: &[(&str, &str)]) -> EnvLookup {
    let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Arc::new(move |name| map.get(name).cloned())
}
