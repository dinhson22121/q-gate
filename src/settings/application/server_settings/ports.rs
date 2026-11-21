use std::sync::Arc;

use crate::settings::domain::{ServerConfig, ValidationError};

pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSettings {
    pub version: i64,
    pub changed_by: String,
    pub created_at: String,
    pub config: ServerConfig,
}

pub trait ServerConfigStore: Send + Sync {
    fn load_raw(&self) -> Result<Option<ServerConfig>, SettingsError>;
    fn save_raw(&self, config: &ServerConfig, changed_by: &str) -> Result<i64, SettingsError>;
    fn seed(&self, config: &ServerConfig, changed_by: &str) -> Result<Option<i64>, SettingsError>;
    fn version(&self, version: i64) -> Result<Option<StoredSettings>, SettingsError>;
    fn recent(&self, limit: u32) -> Result<Vec<StoredSettings>, SettingsError>;
}

pub trait SeedConfigSource: Send + Sync {
    fn origin(&self) -> &str;
    fn load(&self) -> Result<Option<ServerConfig>, SettingsError>;
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(ValidationError),
    #[error("{field} is set by the environment variable {env_var}; change it in qgate.env and restart qgate")]
    Locked { field: String, env_var: &'static str },
    #[error("{field} is admin-only and cannot be changed over HTTP: set {env_var} in qgate.env and restart qgate")]
    AdminOnly { field: String, env_var: &'static str },
    #[error("{name} comes from the environment variable {env_var}; change or remove it in qgate.env and restart qgate")]
    SecretFromEnv { name: &'static str, env_var: &'static str },
    #[error("invalid secret: {0}")]
    InvalidSecret(&'static str),
    #[error("settings version {0} does not exist")]
    VersionNotFound(i64),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Storage(String),
}
