//! Settings domain: configuration models, their defaults and validation rules, and secret types.

pub mod agents;
pub mod config;
pub mod credentials;
pub mod jira;
pub mod output;
pub mod server;
pub mod validation;

pub use agents::{
    AgentEffort, AgentsConfig, ArchitectureConfig, DesignCheck, FailAction, FigmaConfig, FigmaProvider,
    TestAgentConfig, TestLanguage, TicketAnalystConfig,
};
pub use config::{
    BUILTIN_IGNORE_PATTERNS, CheckConfig, Config, Effort, GateConfig, LlmConfig, ReportConfig, ReviewConfig,
    WatchConfig,
};
pub use credentials::{CredentialName, Secret};
pub use jira::{JiraConfig, JiraMcpConfig, JiraOverride, JiraProvider};
pub use output::{OUTPUT_PLACEHOLDERS, OutputConfig, OutputFormat, validate_path_template};
pub use server::{
    ADMIN_ONLY_FIELDS, DashboardConfig, ENV_OVERRIDES, EnvOverride, NotifyConfig, RESTART_REQUIRED_FIELDS,
    RUN_REPO_CHECKS_FIELD, ReposConfig, ServerConfig, ServerLlmConfig, ServerWatchConfig, WEBHOOK_URL_FIELD,
    active_env_overrides, admin_env_var,
};
pub use validation::ValidationError;
