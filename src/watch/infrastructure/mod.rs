//! Adapters of the watch context: git, repository registries, notifiers, instance guard.

pub mod composite_registry;
pub mod git_inspector;
pub mod instance_guard;
pub mod log_notifier;
pub mod scan_registry;
pub mod toml_registry;
pub mod webhook_notifier;

mod git;
