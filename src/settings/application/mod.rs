pub mod credentials;
pub mod server_settings;

pub use crate::settings::domain::{
    AgentEffort, AgentsConfig, CheckConfig, Config, CredentialName, DesignCheck, Effort, FailAction, FigmaConfig,
    FigmaProvider, JiraConfig, JiraMcpConfig, JiraOverride, JiraProvider, OUTPUT_PLACEHOLDERS, OutputConfig,
    OutputFormat, Secret, ServerConfig, ServerLlmConfig, TestLanguage, ValidationError,
};
pub use credentials::{CredentialError, CredentialStore, delete_secret, resolve, store_secret};
pub use server_settings::{
    EnvLookup, LoadEffectiveSettings, SeedConfigSource, ServerConfigStore, SettingsDeps, SettingsError, StoredSettings,
};

pub type SharedServerConfig = std::sync::Arc<std::sync::RwLock<ServerConfig>>;
