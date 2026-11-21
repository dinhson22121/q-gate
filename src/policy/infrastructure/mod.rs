//! Adapters of the policy context: Claude model, SQLite proposal store, and gateways to the settings,
//! conventions and history contexts (through their application layers).

pub mod claude_model;
pub mod history_source;
pub mod rulebook_gateway;
pub mod settings_gateway;
pub mod sqlite;

pub use claude_model::ClaudePolicyModel;
pub use history_source::HistoryUseCases;
pub use rulebook_gateway::RulebookUseCases;
pub use settings_gateway::SettingsUseCases;
pub use sqlite::SqliteProposalStore;
