//! Ports and error type of the server-settings use cases.

use std::sync::Arc;

use crate::settings::domain::{ServerConfig, ValidationError};

/// Reads environment variables (the process env in production, a map in tests).
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// One saved version of the server settings.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSettings {
    pub version: i64,
    pub changed_by: String,
    /// RFC 3339 timestamp.
    pub created_at: String,
    pub config: ServerConfig,
}

/// Append-only, versioned storage of the RAW server settings (env overrides never baked in).
pub trait ServerConfigStore: Send + Sync {
    /// The latest saved settings, or `None` before the first save.
    fn load_raw(&self) -> Result<Option<ServerConfig>, SettingsError>;
    /// Appends `config` as the new latest version and returns its version number.
    fn save_raw(&self, config: &ServerConfig, changed_by: &str) -> Result<i64, SettingsError>;
    /// Saves `config` only if nothing has been saved yet (atomic); returns the version when it did.
    fn seed(&self, config: &ServerConfig, changed_by: &str) -> Result<Option<i64>, SettingsError>;
    fn version(&self, version: i64) -> Result<Option<StoredSettings>, SettingsError>;
    /// The newest `limit` versions, newest first.
    fn recent(&self, limit: u32) -> Result<Vec<StoredSettings>, SettingsError>;
}

/// The legacy `qgate.toml`, read only to seed an empty settings store.
pub trait SeedConfigSource: Send + Sync {
    /// Origin recorded as `changed_by` when imported, e.g. `import:qgate.toml`.
    fn origin(&self) -> &str;
    /// The file's settings, or `None` when there is no file.
    fn load(&self) -> Result<Option<ServerConfig>, SettingsError>;
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(ValidationError),
    #[error("{field} is set by the environment variable {env_var}; change it in qgate.env and restart qgate")]
    Locked { field: String, env_var: &'static str },
    /// An admin-only field (endpoint, command, webhook, enabling repository checks) cannot be changed here.
    #[error("{field} is admin-only and cannot be changed over HTTP: set {env_var} in qgate.env and restart qgate")]
    AdminOnly { field: String, env_var: &'static str },
    #[error("{name} comes from the environment variable {env_var}; change or remove it in qgate.env and restart qgate")]
    SecretFromEnv { name: &'static str, env_var: &'static str },
    #[error("invalid secret: {0}")]
    InvalidSecret(&'static str),
    #[error("settings version {0} does not exist")]
    VersionNotFound(i64),
    /// The settings changed since the caller read them.
    #[error("{0}")]
    Conflict(String),
    /// Failure of a store or file; the message never contains secret values.
    #[error("{0}")]
    Storage(String),
}
