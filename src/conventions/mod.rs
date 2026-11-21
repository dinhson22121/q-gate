//! Conventions bounded context: derive a repository's rules once into a cached [`Rulebook`],
//! let humans approve or reject them, and check every commit cheaply against the approved ones.

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;

use std::path::Path;
use std::sync::Arc;

use crate::llm::ClaudeClient;
use application::{ConventionsError, RulebookReviewer, RulebookService, RulebookStore};
use infrastructure::{ClaudeRuleModel, FsRepoReader, SqliteRulebookStore};

pub use api::{RulebookApiState, router};
pub use domain::rulebook::Rulebook;

/// Composition root: one rulebook service shared by the reviewer and the dashboard API.
#[derive(Clone)]
pub struct Conventions {
    service: Arc<RulebookService>,
}

impl Conventions {
    /// Opens the conventions tables in `db_path` (`<data_dir>/qgate.db`) and uses `llm` for rulebook
    /// builds and checks.
    pub fn open(db_path: &Path, llm: Arc<ClaudeClient>) -> Result<Self, ConventionsError> {
        Ok(Self::with_store(Arc::new(SqliteRulebookStore::open(db_path)?), llm))
    }

    pub fn with_store(store: Arc<dyn RulebookStore>, llm: Arc<ClaudeClient>) -> Self {
        let model = Arc::new(ClaudeRuleModel::new(llm));
        Self { service: Arc::new(RulebookService::new(model, Arc::new(FsRepoReader), store)) }
    }

    /// `use_inferred_rules` / `refresh_days` from `agents.architecture`.
    pub fn reviewer(&self, use_inferred_rules: bool, refresh_days: u32) -> RulebookReviewer {
        RulebookReviewer::with_service(self.service.clone(), use_inferred_rules, refresh_days)
    }

    /// State for [`router`]; `allowed_hosts` = the dashboard's allowed hosts, `token` = its API token.
    pub fn api_state(&self, allowed_hosts: Vec<String>, token: crate::web::ApiToken) -> RulebookApiState {
        RulebookApiState::new(self.service.clone(), allowed_hosts, token)
    }
}

impl RulebookReviewer {
    /// Reviewer on the real adapters (Claude, the repository working tree, `store`). Build locks are
    /// process-wide, so reviewers created per review never build the same repository twice at once.
    pub fn new(
        store: Arc<dyn RulebookStore>,
        builder_llm: Arc<ClaudeClient>,
        use_inferred_rules: bool,
        refresh_days: u32,
    ) -> Self {
        Conventions::with_store(store, builder_llm).reviewer(use_inferred_rules, refresh_days)
    }
}
