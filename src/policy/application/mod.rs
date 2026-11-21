//! Policy use cases and the ports they need. Other contexts use the policy context only through here.

pub mod ports;
pub mod service;

#[cfg(test)]
pub(crate) mod fakes;
#[cfg(test)]
mod tests;

pub use ports::{
    FindingContext, GatePolicySource, PolicyError, PolicyModel, PolicyPrompt, ProposalStore, RepoGate, RuleWrite,
    RulebookGateway, RunHistory, SettingsApplied, SettingsGateway, SettingsSnapshot,
};
pub use service::{
    CHANGED_BY_PREFIX, DEFAULT_DRY_RUN_RUNS, MAX_DRY_RUN_RUNS, PolicyDeps, PolicyService, Prefill, ProposalSummary,
    ProposalView,
};
