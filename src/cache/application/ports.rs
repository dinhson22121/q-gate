pub use crate::cache::domain::ContentKey;

use futures::future::BoxFuture;

#[derive(Debug, thiserror::Error)]
#[error("cache error: {0}")]
pub struct CacheError(pub String);

pub trait ResultCache: Send + Sync {
    fn get(&self, key: &ContentKey) -> Result<Option<Vec<u8>>, CacheError>;

    fn put(&self, key: &ContentKey, value: &[u8]) -> Result<(), CacheError>;
}

pub trait ContentIds: Send + Sync {
    fn object_id<'a>(&'a self, rev: &'a str, path: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>>;

    fn tree_id<'a>(&'a self, rev: &'a str, dir: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>>;
}
