//! Cache use cases and the port they depend on. Other contexts use the cache only through here.

pub mod ports;
pub mod store;

pub use ports::{CacheError, ContentIds, ContentKey, ResultCache};
pub use store::{CacheStore, CacheTags, digest};
