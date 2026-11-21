use crate::cache::application::{CacheError, ContentKey, ResultCache};

pub struct UnavailableCache;

impl ResultCache for UnavailableCache {
    fn get(&self, _key: &ContentKey) -> Result<Option<Vec<u8>>, CacheError> {
        Ok(None)
    }

    fn put(&self, _key: &ContentKey, _value: &[u8]) -> Result<(), CacheError> {
        Ok(())
    }
}
