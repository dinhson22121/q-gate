use std::path::Path;
use std::sync::Arc;

use crate::cache::application::{CacheTags, ContentIds, ResultCache};
use crate::cache::infrastructure::{SqliteResultCache, UnavailableCache};
use crate::impact::application::BaseRunStore;
use crate::impact::infrastructure::CachedBaseRuns;
use crate::review::application::AgentSuite;
use crate::testing::CachingTestAgent;

pub struct ReviewCache {
    pub cache: Arc<dyn ResultCache>,
    pub no_cache: bool,
}

impl ReviewCache {
    pub fn open(db: &Path, requested_no_cache: bool) -> Self {
        match SqliteResultCache::open(db) {
            Ok(cache) => Self { cache: Arc::new(cache), no_cache: requested_no_cache },
            Err(e) => {
                eprintln!("qgate: warning: result cache unavailable, continuing without it: {e}");
                Self { cache: Arc::new(UnavailableCache), no_cache: true }
            }
        }
    }
}

impl ReviewCache {
    pub fn base_runs(&self) -> Option<Arc<dyn BaseRunStore>> {
        (!self.no_cache).then(|| Arc::new(CachedBaseRuns::new(self.cache.clone())) as Arc<dyn BaseRunStore>)
    }
}

pub fn wrap_test_agent(suite: &mut AgentSuite, cache: &ReviewCache, ids: Arc<dyn ContentIds>, tags: CacheTags) {
    suite.tests = suite
        .tests
        .take()
        .map(|inner| Arc::new(CachingTestAgent::new(inner, cache.cache.clone(), ids, tags, cache.no_cache)) as _);
}

pub fn cache_tags(model: &str, effort: &str, prompt: String) -> CacheTags {
    CacheTags {
        model: model.to_string(),
        effort: effort.to_string(),
        prompt,
        toolchain: env!("CARGO_PKG_VERSION").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::application::{CacheError, ContentKey};

    struct Stub;

    impl crate::review::application::agent_ports::TestAgent for Stub {
        fn run<'a>(
            &'a self,
            _input: &'a crate::review::application::agent_ports::AgentInput<'a>,
            _scenario: &'a crate::review::application::TestScenario,
        ) -> futures::future::BoxFuture<'a, crate::review::application::TestResult> {
            Box::pin(std::future::pending())
        }
    }

    #[test]
    fn test_agent_is_wrapped_only_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ReviewCache::open(&dir.path().join("c.db"), false);
        let ids: Arc<dyn ContentIds> = Arc::new(NoIds);
        let mut suite = AgentSuite::default();
        wrap_test_agent(&mut suite, &cache, ids.clone(), cache_tags("m", "e", "p".into()));
        assert!(suite.tests.is_none());
        let inner: Arc<dyn crate::review::application::agent_ports::TestAgent> = Arc::new(Stub);
        suite.tests = Some(inner.clone());
        wrap_test_agent(&mut suite, &cache, ids, cache_tags("m", "e", "p".into()));
        assert!(!Arc::ptr_eq(suite.tests.as_ref().unwrap(), &inner));
    }

    struct NoIds;

    impl ContentIds for NoIds {
        fn object_id<'a>(
            &'a self,
            _rev: &'a str,
            _path: &'a str,
        ) -> futures::future::BoxFuture<'a, Result<Option<String>, CacheError>> {
            Box::pin(async { Ok(None) })
        }

        fn tree_id<'a>(
            &'a self,
            _rev: &'a str,
            _dir: &'a str,
        ) -> futures::future::BoxFuture<'a, Result<Option<String>, CacheError>> {
            Box::pin(async { Ok(None) })
        }

        fn file_text<'a>(
            &'a self,
            _rev: &'a str,
            _path: &'a str,
        ) -> futures::future::BoxFuture<'a, Result<Option<String>, CacheError>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[test]
    fn tags_carry_model_effort_prompt_and_version() {
        let tags = cache_tags("m", "high", "fp".into());
        assert_eq!((tags.model.as_str(), tags.effort.as_str(), tags.prompt.as_str()), ("m", "high", "fp"));
        assert_eq!(tags.toolchain, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn unopenable_database_degrades_to_a_bypassed_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ReviewCache::open(dir.path(), false);
        assert!(cache.no_cache);
        let key = ContentKey::new("k", 1);
        assert_eq!(cache.cache.get(&key).unwrap(), None);
        assert!(cache.cache.put(&key, b"x").is_ok());
    }

    #[test]
    fn base_run_store_is_attached_unless_the_cache_is_bypassed() {
        use crate::impact::application::BaseRunKey;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.db");
        let key = BaseRunKey { base_sha: "a".repeat(40), packages: vec!["m/a".into()] };
        let store = ReviewCache::open(&db, false).base_runs().expect("cache on: store attached");
        store.put(&key, &Default::default());
        assert!(store.get(&key).is_some());
        assert!(ReviewCache::open(&db, true).base_runs().is_none(), "no-cache trailer bypasses the store");
        assert!(ReviewCache::open(dir.path(), false).base_runs().is_none(), "unavailable cache bypasses the store");
    }

    #[test]
    fn openable_database_keeps_the_requested_bypass() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.db");
        assert!(!ReviewCache::open(&db, false).no_cache);
        assert!(ReviewCache::open(&db, true).no_cache);
    }
}
