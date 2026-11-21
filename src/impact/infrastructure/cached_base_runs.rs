use std::sync::Arc;

use crate::cache::application::{ContentKey, ResultCache};
use crate::impact::application::{BaseRunKey, BaseRunStore};
use crate::impact::domain::TestRun;

const NAMESPACE: &str = "impact.base-tests";
const SCHEMA_VERSION: u32 = 1;

pub struct CachedBaseRuns {
    cache: Arc<dyn ResultCache>,
}

impl CachedBaseRuns {
    pub fn new(cache: Arc<dyn ResultCache>) -> Self {
        Self { cache }
    }

    fn content_key(key: &BaseRunKey) -> ContentKey {
        ContentKey::new(NAMESPACE, SCHEMA_VERSION).with_inputs(key.content_inputs())
    }
}

impl BaseRunStore for CachedBaseRuns {
    fn get(&self, key: &BaseRunKey) -> Option<TestRun> {
        match self.cache.get(&Self::content_key(key)) {
            Ok(Some(bytes)) => serde_json::from_slice(&bytes)
                .map_err(|e| eprintln!("qgate: ignoring undecodable base test run: {e}"))
                .ok(),
            Ok(None) => None,
            Err(e) => {
                eprintln!("qgate: base test run cache read failed: {e}");
                None
            }
        }
    }

    fn put(&self, key: &BaseRunKey, run: &TestRun) {
        let bytes = match serde_json::to_vec(run) {
            Ok(bytes) => bytes,
            Err(e) => return eprintln!("qgate: cannot encode base test run: {e}"),
        };
        if let Err(e) = self.cache.put(&Self::content_key(key), &bytes) {
            eprintln!("qgate: base test run cache write failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::cache::application::CacheError;
    use crate::cache::infrastructure::SqliteResultCache;
    use crate::impact::domain::{TestOutcome, TestResult};

    fn key(sha: &str) -> BaseRunKey {
        BaseRunKey { base_sha: sha.into(), packages: vec!["m/a".into(), "m/b".into()] }
    }

    fn run() -> TestRun {
        let mut run = TestRun::default();
        run.insert("m/a", "TestX", TestResult { outcome: TestOutcome::Pass, output_tail: String::new() });
        run
    }

    #[test]
    fn round_trips_through_the_sqlite_cache_and_keys_by_sha_and_packages() {
        let dir = tempfile::tempdir().unwrap();
        let store = CachedBaseRuns::new(Arc::new(SqliteResultCache::open(dir.path().join("c.db")).unwrap()));
        assert_eq!(store.get(&key("aaa")), None);
        store.put(&key("aaa"), &run());
        assert_eq!(store.get(&key("aaa")), Some(run()));
        assert_eq!(store.get(&key("bbb")), None, "another base commit is a miss");
        let other = BaseRunKey { packages: vec!["m/a".into()], ..key("aaa") };
        assert_eq!(store.get(&other), None, "another package set is a miss");
    }

    #[derive(Default)]
    struct Faulty {
        fail: bool,
        garbage: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl ResultCache for Faulty {
        fn get(&self, key: &ContentKey) -> Result<Option<Vec<u8>>, CacheError> {
            if self.fail {
                return Err(CacheError("disk gone".into()));
            }
            Ok(self.garbage.lock().unwrap().get(&key.fingerprint()).cloned())
        }

        fn put(&self, key: &ContentKey, value: &[u8]) -> Result<(), CacheError> {
            if self.fail {
                return Err(CacheError("disk gone".into()));
            }
            self.garbage.lock().unwrap().insert(key.fingerprint(), value.to_vec());
            Ok(())
        }
    }

    #[test]
    fn storage_errors_are_misses_and_never_panic() {
        let store = CachedBaseRuns::new(Arc::new(Faulty { fail: true, ..Faulty::default() }));
        store.put(&key("aaa"), &run());
        assert_eq!(store.get(&key("aaa")), None);
    }

    #[test]
    fn undecodable_entries_are_misses() {
        let faulty = Arc::new(Faulty::default());
        let store = CachedBaseRuns::new(faulty.clone());
        faulty.put(&CachedBaseRuns::content_key(&key("aaa")), b"not json").unwrap();
        assert_eq!(store.get(&key("aaa")), None);
    }
}
