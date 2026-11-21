//! In-memory test doubles for the conventions ports.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;

use super::ports::{
    CheckRequest, ConventionsError, DraftRequest, RepoReader, RuleDecision, RuleModel, RulebookStore, ViolationDraft,
};
use crate::conventions::domain::rulebook::{RuleDraft, RuleStatus, Rulebook};

/// Files keyed by repository-relative path, the same for every root.
#[derive(Default)]
pub struct MemoryRepo {
    pub files: Mutex<HashMap<String, String>>,
}

impl MemoryRepo {
    pub fn with(files: &[(&str, &str)]) -> Self {
        Self { files: Mutex::new(files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()) }
    }

    pub fn set(&self, path: &str, content: &str) {
        self.files.lock().unwrap().insert(path.into(), content.into());
    }
}

impl RepoReader for MemoryRepo {
    fn list_files(&self, _root: &Path) -> Result<Vec<String>, ConventionsError> {
        let mut paths: Vec<String> = self.files.lock().unwrap().keys().cloned().collect();
        paths.sort();
        Ok(paths)
    }

    fn read_capped(&self, _root: &Path, path: &str, cap: usize) -> Result<Option<String>, ConventionsError> {
        Ok(self.files.lock().unwrap().get(path).map(|c| c.chars().take(cap).collect()))
    }
}

#[derive(Default)]
pub struct MemoryStore {
    pub books: Mutex<HashMap<String, Rulebook>>,
    pub decisions: Mutex<HashMap<(String, String), RuleStatus>>,
}

impl RulebookStore for MemoryStore {
    fn load(&self, repo: &str) -> Result<Option<Rulebook>, ConventionsError> {
        Ok(self.books.lock().unwrap().get(repo).cloned())
    }

    fn save(&self, rulebook: &Rulebook) -> Result<(), ConventionsError> {
        self.books.lock().unwrap().insert(rulebook.repo.clone(), rulebook.clone());
        Ok(())
    }

    fn save_with_decisions(&self, rulebook: &Rulebook, decisions: &[RuleDecision]) -> Result<(), ConventionsError> {
        self.save(rulebook)?;
        let mut stored = self.decisions.lock().unwrap();
        for decision in decisions {
            stored.insert((rulebook.repo.clone(), decision.normalized_title.clone()), decision.status);
        }
        Ok(())
    }

    fn decisions(&self, repo: &str) -> Result<HashMap<String, RuleStatus>, ConventionsError> {
        let all = self.decisions.lock().unwrap();
        Ok(all.iter().filter(|((r, _), _)| r == repo).map(|((_, t), s)| (t.clone(), *s)).collect())
    }

    fn repos(&self) -> Result<Vec<String>, ConventionsError> {
        let mut repos: Vec<String> = self.books.lock().unwrap().keys().cloned().collect();
        repos.sort();
        Ok(repos)
    }
}

/// Returns canned drafts / violations, counts calls and records the last requests.
#[derive(Default)]
pub struct FakeModel {
    pub drafts: Mutex<Vec<RuleDraft>>,
    pub violations: Mutex<Vec<ViolationDraft>>,
    pub draft_calls: AtomicUsize,
    pub check_calls: AtomicUsize,
    pub delay: Option<Duration>,
    pub fail: bool,
    pub last_draft: Mutex<Option<DraftRequest>>,
    pub last_selected: Mutex<Vec<String>>,
    pub last_rule_ids: Mutex<Vec<String>>,
}

impl FakeModel {
    pub fn drafting(drafts: Vec<RuleDraft>) -> Self {
        Self { drafts: Mutex::new(drafts), ..Self::default() }
    }

    pub fn draft_calls(&self) -> usize {
        self.draft_calls.load(Ordering::SeqCst)
    }
}

impl RuleModel for FakeModel {
    fn draft_rules<'a>(&'a self, request: &'a DraftRequest) -> BoxFuture<'a, Result<Vec<RuleDraft>, ConventionsError>> {
        Box::pin(async move {
            self.draft_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_draft.lock().unwrap() = Some(request.clone());
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            if self.fail {
                return Err(ConventionsError::Model("model down".into()));
            }
            Ok(self.drafts.lock().unwrap().clone())
        })
    }

    fn find_violations<'a>(
        &'a self,
        request: CheckRequest<'a>,
    ) -> BoxFuture<'a, Result<Vec<ViolationDraft>, ConventionsError>> {
        Box::pin(async move {
            self.check_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_selected.lock().unwrap() = request.selected.iter().map(|s| s.to_string()).collect();
            *self.last_rule_ids.lock().unwrap() = request.rules.iter().map(|r| r.id.clone()).collect();
            Ok(self.violations.lock().unwrap().clone())
        })
    }
}
