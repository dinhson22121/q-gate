//! Ports of the cache context. Infrastructure adapters implement [`ResultCache`]; the key type is
//! re-exported so callers depend only on the application layer.

pub use crate::cache::domain::ContentKey;

use futures::future::BoxFuture;

/// A cache lookup or write failed (storage unavailable, corruption...).
#[derive(Debug, thiserror::Error)]
#[error("cache error: {0}")]
pub struct CacheError(pub String);

/// Stores opaque byte payloads keyed by the content hash of the inputs that produced them.
///
/// A result may be reused only when its [`ContentKey::fingerprint`] matches, so any change to an
/// input yields a miss. Callers serialize their own value (for example with `serde_json`) into the
/// bytes they `put`.
pub trait ResultCache: Send + Sync {
    /// The stored payload for `key`, or `None` on a miss.
    fn get(&self, key: &ContentKey) -> Result<Option<Vec<u8>>, CacheError>;

    /// Stores `value` under `key`, overwriting any existing payload for the same fingerprint.
    fn put(&self, key: &ContentKey, value: &[u8]) -> Result<(), CacheError>;
}

/// Content-addressed object ids of a repository at a revision (git blob / tree ids). Lets callers
/// key a result on the exact content it was derived from without reading that content.
pub trait ContentIds: Send + Sync {
    /// Id of the blob or tree at `path` in `rev`; `None` when the path does not exist there.
    fn object_id<'a>(&'a self, rev: &'a str, path: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>>;

    /// Id of the tree at `dir` in `rev` (`""` is the root tree); `None` when `dir` does not exist there.
    fn tree_id<'a>(&'a self, rev: &'a str, dir: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>>;
}
