//! Composition of the result cache: one SQLite cache per review, the `qgate: no-cache` trailer and the key tags.
//! Caching is transparent: a cache that cannot be opened degrades to "never hits" and never fails the review.

use std::path::Path;
use std::sync::Arc;

use crate::cache::application::{CacheError, CacheTags, ContentIds, ContentKey, ResultCache};
use crate::cache::infrastructure::SqliteResultCache;
use crate::impact::application::BaseRunStore;
use crate::impact::infrastructure::CachedBaseRuns;
use crate::review::application::AgentSuite;
use crate::testing::CachingTestAgent;

const TRAILER_KEY: &str = "qgate";
const TRAILER_VALUE: &str = "no-cache";

/// The cache to use and whether it must be bypassed (explicit request or storage unavailable).
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
    /// Store for the base commit's test run; `None` when caching is bypassed so the base tests always re-run.
    pub fn base_runs(&self) -> Option<Arc<dyn BaseRunStore>> {
        (!self.no_cache).then(|| Arc::new(CachedBaseRuns::new(self.cache.clone())) as Arc<dyn BaseRunStore>)
    }
}

/// Stand-in used when the cache database cannot be opened: always misses, stores nothing.
struct UnavailableCache;

impl ResultCache for UnavailableCache {
    fn get(&self, _key: &ContentKey) -> Result<Option<Vec<u8>>, CacheError> {
        Ok(None)
    }

    fn put(&self, _key: &ContentKey, _value: &[u8]) -> Result<(), CacheError> {
        Ok(())
    }
}

/// Puts the cache in front of the suite's test agent (when there is one).
pub fn wrap_test_agent(suite: &mut AgentSuite, cache: &ReviewCache, ids: Arc<dyn ContentIds>, tags: CacheTags) {
    suite.tests = suite
        .tests
        .take()
        .map(|inner| Arc::new(CachingTestAgent::new(inner, cache.cache.clone(), ids, tags, cache.no_cache)) as _);
}

/// Whether any commit message carries a `qgate: no-cache` trailer (case-insensitive, one per line).
pub fn has_no_cache_trailer(messages: &[String]) -> bool {
    messages.iter().flat_map(|m| m.lines()).any(is_no_cache_line)
}

fn is_no_cache_line(line: &str) -> bool {
    line.split_once(':').is_some_and(|(key, value)| {
        key.trim().eq_ignore_ascii_case(TRAILER_KEY) && value.trim().eq_ignore_ascii_case(TRAILER_VALUE)
    })
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

    fn msgs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn trailer_is_found_case_insensitively_in_any_message() {
        assert!(has_no_cache_trailer(&msgs(&["feat: x\n\nqgate: no-cache"])));
        assert!(has_no_cache_trailer(&msgs(&["fix: y", "feat: x\n\nQGate:   No-Cache  \n"])));
    }

    #[test]
    fn other_text_is_not_a_trailer() {
        assert!(!has_no_cache_trailer(&[]));
        assert!(!has_no_cache_trailer(&msgs(&[
            "feat: no-cache layer",
            "qgate: cache",
            "qgate no-cache",
            "x-qgate: no-cache"
        ])));
        assert!(!has_no_cache_trailer(&msgs(&["qgate: no-cache please"])));
    }

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
        // A directory where the database file should be.
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
