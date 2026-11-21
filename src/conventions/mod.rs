pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;

use std::path::Path;
use std::sync::Arc;

use crate::llm::StructuredModel;
use application::{ConventionsError, RulebookReviewer, RulebookService, RulebookStore};
use infrastructure::{ClaudeRuleModel, FsRepoReader, SqliteRulebookStore};

pub use api::{RulebookApiState, router};
pub use domain::rulebook::Rulebook;

#[derive(Clone)]
pub struct Conventions {
    service: Arc<RulebookService>,
}

impl Conventions {
    pub fn open(db_path: &Path, llm: Arc<dyn StructuredModel>) -> Result<Self, ConventionsError> {
        Ok(Self::with_store(Arc::new(SqliteRulebookStore::open(db_path)?), llm))
    }

    pub fn with_store(store: Arc<dyn RulebookStore>, llm: Arc<dyn StructuredModel>) -> Self {
        let model = Arc::new(ClaudeRuleModel::new(llm));
        Self { service: Arc::new(RulebookService::new(model, Arc::new(FsRepoReader), store)) }
    }

    pub fn reviewer(&self, use_inferred_rules: bool, refresh_days: u32) -> RulebookReviewer {
        RulebookReviewer::with_service(self.service.clone(), use_inferred_rules, refresh_days)
    }

    pub fn api_state(&self, allowed_hosts: Vec<String>, token: crate::web::ApiToken) -> RulebookApiState {
        RulebookApiState::new(self.service.clone(), allowed_hosts, token)
    }
}

impl RulebookReviewer {
    pub fn new(
        store: Arc<dyn RulebookStore>,
        builder_llm: Arc<dyn StructuredModel>,
        use_inferred_rules: bool,
        refresh_days: u32,
    ) -> Self {
        Conventions::with_store(store, builder_llm).reviewer(use_inferred_rules, refresh_days)
    }
}
