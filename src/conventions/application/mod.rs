pub mod build;
pub mod changes;
pub mod ports;
pub mod reviewer;
pub mod service;

#[cfg(test)]
pub(crate) mod fakes;
#[cfg(test)]
mod tests;

pub use crate::conventions::domain::rulebook::{Rule, RuleDraft, RuleKind, RuleRef, RuleStatus, Rulebook, TeamRule};
pub use build::{BuildRulebook, repo_key};
pub use ports::{
    CheckRequest, ConventionsError, DraftRequest, RepoReader, RuleDecision, RuleModel, RulebookStore, ViolationDraft,
};
pub use reviewer::RulebookReviewer;
pub use service::{BuildState, RulebookService};
