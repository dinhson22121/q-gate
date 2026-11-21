//! Policy context ("Policy assistant"): a team describes how it reviews code, a model turns that into a
//! structured proposal (server settings, rulebook rules, a `.qgate.toml` patch), the user previews the diff and
//! a dry-run on history, and nothing is applied before an explicit approval. Changes go through the settings
//! and conventions use cases (validation, env locks, versioning).

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;

pub use api::{PolicyApiState, router};
