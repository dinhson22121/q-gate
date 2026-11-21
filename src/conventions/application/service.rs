//! Keeps rulebooks current (one build at a time per repository) and applies human decisions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};

use chrono::Utc;
use tokio::sync::Mutex as AsyncMutex;

use super::build::{BuildRulebook, blocking, repo_key};
use super::ports::{ConventionsError, RuleDecision, RuleModel, RulebookStore};
use crate::conventions::domain::rulebook::{RuleStatus, Rulebook, TeamRule, normalize_title};

/// Progress of the latest build of one repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildState {
    pub building: bool,
    pub last_error: Option<String>,
}

#[derive(Default)]
struct Registry {
    locks: HashMap<String, Arc<AsyncMutex<()>>>,
    states: HashMap<String, BuildState>,
}

/// Process-wide so every service instance (reviewer per review, dashboard API) shares the per-repo
/// build lock: a repository is never built twice at the same time.
static REGISTRY: LazyLock<StdMutex<Registry>> = LazyLock::new(StdMutex::default);

fn with_registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut registry = REGISTRY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut registry)
}

fn build_lock(repo: &str) -> Arc<AsyncMutex<()>> {
    with_registry(|r| r.locks.entry(repo.to_string()).or_default().clone())
}

fn set_state(repo: &str, state: BuildState) {
    with_registry(|r| r.states.insert(repo.to_string(), state));
}

pub struct RulebookService {
    build: BuildRulebook,
    model: Arc<dyn RuleModel>,
    store: Arc<dyn RulebookStore>,
}

impl RulebookService {
    pub fn new(
        model: Arc<dyn RuleModel>,
        reader: Arc<dyn super::ports::RepoReader>,
        store: Arc<dyn RulebookStore>,
    ) -> Self {
        Self { build: BuildRulebook::new(model.clone(), reader, store.clone()), model, store }
    }

    pub(crate) fn model(&self) -> &dyn RuleModel {
        self.model.as_ref()
    }

    /// The stored rulebook if it is still current; otherwise builds one. Concurrent callers for the
    /// same repository wait for a single build.
    pub async fn current(&self, root: &Path, refresh_days: u32) -> Result<Rulebook, ConventionsError> {
        let repo = repo_key(root);
        let lock = build_lock(&repo);
        let _guard = lock.lock().await;
        if let Some(existing) = self.load(&repo).await? {
            let fingerprint = self.build.current_fingerprint(root).await?;
            if !existing.is_stale(&fingerprint, Utc::now(), refresh_days) {
                return Ok(existing);
            }
        }
        self.build_locked(&repo, root).await
    }

    /// Builds a new rulebook even if the current one is fresh.
    pub async fn rebuild(&self, root: &Path) -> Result<Rulebook, ConventionsError> {
        let repo = repo_key(root);
        let lock = build_lock(&repo);
        let _guard = lock.lock().await;
        self.build_locked(&repo, root).await
    }

    async fn build_locked(&self, repo: &str, root: &Path) -> Result<Rulebook, ConventionsError> {
        set_state(repo, BuildState { building: true, last_error: None });
        let result = self.build.execute(root).await;
        let last_error = result.as_ref().err().map(ToString::to_string);
        if let Some(error) = &last_error {
            eprintln!("qgate conventions: rulebook build for {repo} failed: {error}");
        }
        set_state(repo, BuildState { building: false, last_error });
        result
    }

    /// Starts a rebuild of a repository that already has a rulebook (never an arbitrary path) in the
    /// background. Returns at once; progress is visible through [`Self::build_state`].
    pub async fn start_rebuild(self: &Arc<Self>, repo: &str) -> Result<(), ConventionsError> {
        if self.load(repo).await?.is_none() {
            return Err(ConventionsError::NotFound(format!("no rulebook for repository {repo}")));
        }
        set_state(repo, BuildState { building: true, last_error: None });
        let service = self.clone();
        let root = PathBuf::from(repo);
        tokio::spawn(async move {
            // Errors are recorded in the build state and logged by `build_locked`.
            let _ = service.rebuild(&root).await;
        });
        Ok(())
    }

    pub fn build_state(&self, repo: &str) -> BuildState {
        with_registry(|r| r.states.get(repo).cloned().unwrap_or_default())
    }

    pub async fn load(&self, repo: &str) -> Result<Option<Rulebook>, ConventionsError> {
        let store = self.store.clone();
        let repo = repo.to_string();
        blocking(move || store.load(&repo)).await
    }

    pub async fn repos(&self) -> Result<Vec<String>, ConventionsError> {
        let store = self.store.clone();
        blocking(move || store.repos()).await
    }

    /// Records a human decision on one rule; it survives rebuilds (matched by normalized title).
    pub async fn decide(&self, repo: &str, rule_id: &str, status: RuleStatus) -> Result<Rulebook, ConventionsError> {
        let store = self.store.clone();
        let (repo, rule_id) = (repo.to_string(), rule_id.to_string());
        blocking(move || {
            let not_found = || ConventionsError::NotFound(format!("no rule {rule_id} in the rulebook of {repo}"));
            let rulebook = store.load(&repo)?.ok_or_else(not_found)?;
            let rule = rulebook.rule(&rule_id).ok_or_else(not_found)?;
            let decision =
                RuleDecision { normalized_title: normalize_title(&rule.title), status, decided_at: Utc::now() };
            let updated = rulebook.with_status(&rule_id, status).ok_or_else(not_found)?;
            store.save_with_decision(&updated, &decision)?;
            Ok(updated)
        })
        .await
    }

    /// Adds or edits team rules (Policy assistant) with their statuses recorded as decisions, so both
    /// survive rebuilds. Creates the repository's rulebook when there is none yet. Waits for a running build.
    pub async fn add_team_rules(&self, repo: &str, writes: Vec<TeamRule>) -> Result<Rulebook, ConventionsError> {
        let lock = build_lock(repo);
        let _guard = lock.lock().await;
        let store = self.store.clone();
        let repo = repo.to_string();
        blocking(move || {
            let current = store.load(&repo)?.unwrap_or_else(|| Rulebook::empty(repo.clone(), Utc::now()));
            let (updated, decisions) =
                current.with_team_rules(&writes).map_err(|e| ConventionsError::Conflict(e.to_string()))?;
            let decided_at = Utc::now();
            let decisions: Vec<RuleDecision> = decisions
                .into_iter()
                .map(|(normalized_title, status)| RuleDecision { normalized_title, status, decided_at })
                .collect();
            store.save_with_decisions(&updated, &decisions)?;
            Ok(updated)
        })
        .await
    }
}
