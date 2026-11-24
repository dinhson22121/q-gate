use std::path::PathBuf;
use std::sync::Arc;

use super::config_file::read_toml;
use super::paths::AppPaths;
use crate::settings::application::{EnvLookup, SeedConfigSource, SettingsError};
use crate::settings::domain::ServerConfig;

pub const SEED_ORIGIN: &str = "import:qgate.toml";

pub struct TomlSeedSource {
    path: PathBuf,
}

impl TomlSeedSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_paths(paths: &AppPaths) -> Self {
        Self::new(paths.server_config_file())
    }
}

impl SeedConfigSource for TomlSeedSource {
    fn origin(&self) -> &str {
        SEED_ORIGIN
    }

    fn load(&self) -> Result<Option<ServerConfig>, SettingsError> {
        if !self.path.exists() {
            return Ok(None);
        }
        read_toml::<ServerConfig>(&self.path).map(Some).map_err(|e| SettingsError::Storage(e.to_string()))
    }
}

pub fn process_env() -> EnvLookup {
    Arc::new(|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_no_seed_and_bad_toml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qgate.toml");
        let seed = TomlSeedSource::new(&path);
        assert_eq!(seed.origin(), SEED_ORIGIN);
        assert_eq!(seed.load().unwrap(), None);
        std::fs::write(&path, "[repos]\nscan_depth = 4\n").unwrap();
        assert_eq!(seed.load().unwrap().unwrap().repos.scan_depth, 4);
        std::fs::write(&path, "[repos]\nnope = 1\n").unwrap();
        assert!(seed.load().unwrap_err().to_string().contains("qgate.toml"));
        let _ = TomlSeedSource::from_paths(&AppPaths::with_dirs(dir.path(), dir.path()));
        assert!(process_env()("QGATE_SURELY_UNSET_VAR_123").is_none());
    }

    #[test]
    fn first_start_imports_the_file_into_sqlite_once() {
        use crate::settings::application::{LoadEffectiveSettings, ServerConfigStore};
        let dir = tempfile::tempdir().unwrap();
        let toml = dir.path().join("qgate.toml");
        std::fs::write(&toml, "[watch]\npoll_interval_secs = 7\n").unwrap();
        let store = Arc::new(super::super::SqliteServerConfigStore::open(dir.path().join("qgate.db")).unwrap());
        let env: EnvLookup =
            Arc::new(|name| (name == "JIRA_BASE_URL").then(|| "https://acme.atlassian.net".to_string()));
        let load = LoadEffectiveSettings::new(store.clone(), Arc::new(TomlSeedSource::new(&toml)), env);
        let effective = load.execute().unwrap();
        assert_eq!(
            (effective.watch.poll_interval_secs, effective.jira.base_url.as_str()),
            (7, "https://acme.atlassian.net")
        );
        std::fs::write(&toml, "[watch]\npoll_interval_secs = 9\n").unwrap();
        assert_eq!(load.execute().unwrap().watch.poll_interval_secs, 7, "file not read after import");
        let rows = store.recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].changed_by, SEED_ORIGIN);
        assert_ne!(rows[0].config.jira.base_url, "https://acme.atlassian.net", "env value not persisted");
    }
}
