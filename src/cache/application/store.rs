//! Typed, async access to a [`ResultCache`] for callers that cache serializable values: JSON
//! encoding, the blocking SQLite call moved off the async runtime, the `no_cache` opt-out and the
//! rule that a cache problem is a miss, never a failure of the review.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::ports::{ContentKey, ResultCache};

/// Hex SHA-256 of `parts`, each length-prefixed so no concatenation can collide with another.
pub fn digest<S: AsRef<[u8]>>(parts: impl IntoIterator<Item = S>) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        let bytes = part.as_ref();
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Everything besides the code that changes what a model-backed step produces. All of it goes into
/// the key, so changing any one of them is a miss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheTags {
    pub model: String,
    pub effort: String,
    /// Fingerprint of the prompts in use (see the `prompt_fingerprint` of the producing context).
    pub prompt: String,
    /// Toolchain / environment version (qgate version, language toolchains...).
    pub toolchain: String,
}

impl CacheTags {
    /// Adds the tags to `key` as inputs.
    #[must_use]
    pub fn apply(&self, key: ContentKey) -> ContentKey {
        key.with_input("tag:model", &self.model)
            .with_input("tag:effort", &self.effort)
            .with_input("tag:prompt", &self.prompt)
            .with_input("tag:toolchain", &self.toolchain)
    }
}

/// A [`ResultCache`] plus the `no_cache` switch. Cheap to clone.
#[derive(Clone)]
pub struct CacheStore {
    cache: Arc<dyn ResultCache>,
    no_cache: bool,
}

impl CacheStore {
    /// With `no_cache` set, [`get`](Self::get) always misses and [`put`](Self::put) does nothing.
    pub fn new(cache: Arc<dyn ResultCache>, no_cache: bool) -> Self {
        Self { cache, no_cache }
    }

    pub fn is_bypassed(&self) -> bool {
        self.no_cache
    }

    /// The cached value, or `None` on a miss, a bypass, a storage error or an undecodable payload.
    pub async fn get<T: DeserializeOwned + Send + 'static>(&self, key: &ContentKey) -> Option<T> {
        if self.no_cache {
            return None;
        }
        let (cache, key) = (Arc::clone(&self.cache), key.clone());
        let stored = tokio::task::spawn_blocking(move || cache.get(&key)).await;
        match stored {
            Ok(Ok(Some(bytes))) => match serde_json::from_slice(&bytes) {
                Ok(value) => Some(value),
                Err(e) => {
                    eprintln!("qgate: ignoring undecodable cache entry: {e}");
                    None
                }
            },
            Ok(Ok(None)) => None,
            Ok(Err(e)) => {
                eprintln!("qgate: cache read failed: {e}");
                None
            }
            Err(e) => {
                eprintln!("qgate: cache read task failed: {e}");
                None
            }
        }
    }

    /// Stores `value`; failures are reported on stderr and otherwise ignored.
    pub async fn put<T: Serialize>(&self, key: &ContentKey, value: &T) {
        if self.no_cache {
            return;
        }
        let bytes = match serde_json::to_vec(value) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("qgate: cannot encode cache entry: {e}");
                return;
            }
        };
        let (cache, key) = (Arc::clone(&self.cache), key.clone());
        match tokio::task::spawn_blocking(move || cache.put(&key, &bytes)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("qgate: cache write failed: {e}"),
            Err(e) => eprintln!("qgate: cache write task failed: {e}"),
        }
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::super::ports::{CacheError, ContentKey, ResultCache};

    /// In-memory cache counting reads and writes; can be told to fail.
    #[derive(Default)]
    pub struct MemoryCache {
        pub map: Mutex<HashMap<String, Vec<u8>>>,
        pub gets: Mutex<u32>,
        pub puts: Mutex<u32>,
        pub fail: bool,
    }

    impl ResultCache for MemoryCache {
        fn get(&self, key: &ContentKey) -> Result<Option<Vec<u8>>, CacheError> {
            *self.gets.lock().unwrap() += 1;
            if self.fail {
                return Err(CacheError("down".into()));
            }
            Ok(self.map.lock().unwrap().get(&key.fingerprint()).cloned())
        }

        fn put(&self, key: &ContentKey, value: &[u8]) -> Result<(), CacheError> {
            *self.puts.lock().unwrap() += 1;
            if self.fail {
                return Err(CacheError("down".into()));
            }
            self.map.lock().unwrap().insert(key.fingerprint(), value.to_vec());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::MemoryCache;
    use super::*;

    fn key() -> ContentKey {
        ContentKey::new("t", 1).with_input("a", "1")
    }

    fn store(cache: &Arc<MemoryCache>, no_cache: bool) -> CacheStore {
        CacheStore::new(cache.clone(), no_cache)
    }

    #[tokio::test]
    async fn round_trips_a_value_through_json() {
        let cache = Arc::new(MemoryCache::default());
        let store = store(&cache, false);
        store.put(&key(), &vec!["x".to_string(), "y".to_string()]).await;
        assert_eq!(store.get::<Vec<String>>(&key()).await, Some(vec!["x".to_string(), "y".to_string()]));
        assert_eq!(store.get::<Vec<String>>(&ContentKey::new("t", 1)).await, None);
    }

    #[tokio::test]
    async fn no_cache_bypasses_reads_and_writes() {
        let cache = Arc::new(MemoryCache::default());
        let store = store(&cache, true);
        store.put(&key(), &1_u32).await;
        assert_eq!(store.get::<u32>(&key()).await, None);
        assert!(store.is_bypassed());
        assert_eq!((*cache.gets.lock().unwrap(), *cache.puts.lock().unwrap()), (0, 0));
    }

    #[tokio::test]
    async fn storage_errors_and_corrupt_payloads_are_misses() {
        let failing = Arc::new(MemoryCache { fail: true, ..MemoryCache::default() });
        let store_failing = store(&failing, false);
        store_failing.put(&key(), &1_u32).await;
        assert_eq!(store_failing.get::<u32>(&key()).await, None);

        let cache = Arc::new(MemoryCache::default());
        cache.put(&key(), b"not json").unwrap();
        assert_eq!(store(&cache, false).get::<u32>(&key()).await, None);
    }

    #[test]
    fn digest_is_stable_and_boundary_safe() {
        assert_eq!(digest(["ab", "c"]), digest(["ab", "c"]));
        assert_ne!(digest(["ab", "c"]), digest(["a", "bc"]));
        assert_eq!(digest(["x"]).len(), 64);
    }

    #[test]
    fn every_tag_changes_the_fingerprint() {
        let tags = CacheTags { model: "m".into(), effort: "high".into(), prompt: "p".into(), toolchain: "t".into() };
        let base = tags.apply(key()).fingerprint();
        assert_eq!(base, tags.apply(key()).fingerprint());
        for changed in [
            CacheTags { model: "m2".into(), ..tags.clone() },
            CacheTags { effort: "low".into(), ..tags.clone() },
            CacheTags { prompt: "p2".into(), ..tags.clone() },
            CacheTags { toolchain: "t2".into(), ..tags.clone() },
        ] {
            assert_ne!(changed.apply(key()).fingerprint(), base);
        }
    }
}
