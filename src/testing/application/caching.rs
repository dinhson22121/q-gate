use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::cache::application::{CacheError, CacheStore, CacheTags, ContentIds, ContentKey, digest};
use crate::review::application::agent_ports::{AgentInput, TestAgent};
use crate::review::application::{TestResult, TestScenario, TestVerdict};
use crate::testing::domain::dependencies::has_local_dependency;
use crate::testing::domain::verdict::criterion_texts;

const KIND: &str = "test_scenario";
const SCHEMA_VERSION: u32 = 1;
const MANIFESTS: [&str; 13] = [
    "Cargo.toml",
    "go.mod",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "requirements.txt",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "Gemfile",
    "composer.json",
];
const WORKSPACE_FILES: [&str; 1] = ["go.work"];
const ROOT_BUILD_FILES: [&str; 8] = [
    "Cargo.lock",
    "go.sum",
    "go.work",
    "go.work.sum",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredTest {
    commit: String,
    result: TestResult,
}

struct ScenarioKey {
    key: ContentKey,
    covered_dirs: Vec<String>,
}

pub struct CachingTestAgent {
    inner: Arc<dyn TestAgent>,
    store: CacheStore,
    ids: Arc<dyn ContentIds>,
    tags: CacheTags,
}

impl CachingTestAgent {
    pub fn new(
        inner: Arc<dyn TestAgent>,
        cache: Arc<dyn crate::cache::application::ResultCache>,
        ids: Arc<dyn ContentIds>,
        tags: CacheTags,
        no_cache: bool,
    ) -> Self {
        Self { inner, store: CacheStore::new(cache, no_cache), ids, tags }
    }

    async fn scenario_key(&self, input: &AgentInput<'_>, scenario: &TestScenario) -> Option<ScenarioKey> {
        let paths: BTreeSet<&str> = input.files.iter().map(|f| f.path.as_str()).collect();
        let mut modules = HashMap::new();
        let mut covered = BTreeSet::new();
        for path in &paths {
            let dir = self.module_dir(input.commit, path, &mut modules).await.ok()?;
            covered.insert(dir);
        }
        if self.reaches_outside_module(input.commit, &covered).await? {
            return None;
        }
        let mut key = self.tags.apply(ContentKey::new(KIND, SCHEMA_VERSION));
        key = key.with_input("scenario", scenario_digest(input, scenario));
        key = key.with_input("changed", digest(paths.iter().copied()));
        for dir in &covered {
            let id = self.ids.tree_id(input.commit, dir).await.ok()?;
            key = key.with_input(format!("tree:{dir}"), id.unwrap_or_else(|| "absent".into()));
        }
        for name in ROOT_BUILD_FILES.iter().chain(MANIFESTS.iter()) {
            if let Some(id) = self.ids.object_id(input.commit, name).await.ok()? {
                key = key.with_input(format!("root:{name}"), id);
            }
        }
        Some(ScenarioKey { key, covered_dirs: covered.into_iter().collect() })
    }

    async fn reaches_outside_module(&self, commit: &str, covered: &BTreeSet<String>) -> Option<bool> {
        let module_dirs: Vec<&str> = covered.iter().map(String::as_str).filter(|dir| !dir.is_empty()).collect();
        if module_dirs.is_empty() {
            return Some(false);
        }
        for dir in std::iter::once("").chain(module_dirs) {
            for name in MANIFESTS.iter().chain(WORKSPACE_FILES.iter()) {
                let path = if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") };
                let text = self.ids.file_text(commit, &path).await.ok()?;
                if text.is_some_and(|text| has_local_dependency(name, &text)) {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    async fn module_dir(
        &self,
        commit: &str,
        path: &str,
        memo: &mut HashMap<String, String>,
    ) -> Result<String, CacheError> {
        let mut dir = path.rsplit_once('/').map_or("", |(parent, _)| parent).to_string();
        let start = dir.clone();
        if let Some(found) = memo.get(&start) {
            return Ok(found.clone());
        }
        let found = loop {
            if self.has_manifest(commit, &dir).await? {
                break dir;
            }
            match dir.rsplit_once('/') {
                Some((parent, _)) => dir = parent.to_string(),
                None if dir.is_empty() => break String::new(),
                None => dir = String::new(),
            }
        };
        memo.insert(start, found.clone());
        Ok(found)
    }

    async fn has_manifest(&self, commit: &str, dir: &str) -> Result<bool, CacheError> {
        for name in MANIFESTS {
            let path = if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") };
            if self.ids.object_id(commit, &path).await?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn scenario_digest(input: &AgentInput<'_>, scenario: &TestScenario) -> String {
    let texts = criterion_texts(input.requirements, &scenario.criterion_ids);
    let mut parts = vec![scenario.id.clone(), scenario.given.clone(), scenario.when.clone(), scenario.then.clone()];
    parts.extend(scenario.criterion_ids.iter().map(|id| format!("criterion:{id}")));
    parts.extend(texts.into_iter().map(|t| format!("text:{t}")));
    digest(parts)
}

fn is_cacheable(result: &TestResult, covered_dirs: &[String]) -> bool {
    if result.verdict != TestVerdict::Passed {
        return false;
    }
    let Some(evidence) = &result.evidence else { return false };
    !evidence.code_under_test.is_empty()
        && evidence.code_under_test.iter().all(|r| covered_dirs.iter().any(|dir| within(&r.file, dir)))
}

fn within(file: &str, dir: &str) -> bool {
    dir.is_empty() || file.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

impl TestAgent for CachingTestAgent {
    fn run<'a>(&'a self, input: &'a AgentInput<'a>, scenario: &'a TestScenario) -> BoxFuture<'a, TestResult> {
        Box::pin(async move {
            if self.store.is_bypassed() {
                return self.inner.run(input, scenario).await;
            }
            let key = self.scenario_key(input, scenario).await;
            if let Some(key) = &key
                && let Some(stored) = self.store.get::<StoredTest>(&key.key).await
                && stored.result.verdict == TestVerdict::Passed
                && stored.result.scenario_id == scenario.id
            {
                let mut result = stored.result;
                result.note = format!("reused from cache (commit {}); {}", stored.commit, result.note);
                return result;
            }
            let result = self.inner.run(input, scenario).await;
            if let Some(key) = key
                && is_cacheable(&result, &key.covered_dirs)
            {
                self.store
                    .put(&key.key, &StoredTest { commit: input.commit.to_string(), result: result.clone() })
                    .await;
            }
            result
        })
    }
}

#[cfg(test)]
#[path = "caching_tests.rs"]
mod tests;
