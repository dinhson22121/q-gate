//! Combines a preferred store with a fallback (e.g. keychain unavailable over SSH or in CI).

use crate::settings::application::credentials::{CredentialError, CredentialStore};
use crate::settings::domain::credentials::{CredentialName, Secret};

/// Reads from `primary`, then `fallback`; writes to `primary`, or `fallback` when `primary` fails.
#[derive(Debug, Clone)]
pub struct FallbackStore<P, F> {
    primary: P,
    fallback: F,
}

impl<P, F> FallbackStore<P, F> {
    pub fn new(primary: P, fallback: F) -> Self {
        Self { primary, fallback }
    }
}

impl<P: CredentialStore, F: CredentialStore> CredentialStore for FallbackStore<P, F> {
    fn get(&self, name: CredentialName) -> Result<Option<Secret>, CredentialError> {
        match self.primary.get(name) {
            Ok(Some(secret)) => Ok(Some(secret)),
            Ok(None) => self.fallback.get(name),
            // Report the primary failure only when the fallback has nothing either.
            Err(primary_error) => self.fallback.get(name)?.map_or(Err(primary_error), |s| Ok(Some(s))),
        }
    }

    fn set(&self, name: CredentialName, secret: &Secret) -> Result<(), CredentialError> {
        self.primary.set(name, secret).or_else(|_| self.fallback.set(name, secret))
    }

    /// Deletes from both so a stale copy cannot resurface; the first failure is returned.
    fn delete(&self, name: CredentialName) -> Result<(), CredentialError> {
        let primary = self.primary.delete(name);
        let fallback = self.fallback.delete(name);
        primary.and(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::application::credentials::fakes::MemoryStore;

    const KEY: CredentialName = CredentialName::AnthropicApiKey;

    fn secret(v: &str) -> Secret {
        Secret::new(v)
    }

    #[test]
    fn reads_primary_then_fallback() {
        let store = FallbackStore::new(MemoryStore::default(), MemoryStore::default());
        store.fallback.set(KEY, &secret("old")).unwrap();
        assert_eq!(store.get(KEY).unwrap().unwrap().expose(), "old");
        store.set(KEY, &secret("new")).unwrap();
        assert_eq!(store.get(KEY).unwrap().unwrap().expose(), "new");
    }

    #[test]
    fn falls_back_when_primary_fails() {
        let store = FallbackStore::new(MemoryStore::failing(), MemoryStore::default());
        assert!(store.get(KEY).is_err(), "primary error surfaces when fallback is empty");
        store.set(KEY, &secret("v")).unwrap();
        assert_eq!(store.get(KEY).unwrap().unwrap().expose(), "v");
        assert!(store.delete(KEY).is_err());
        assert!(store.fallback.get(KEY).unwrap().is_none(), "fallback copy removed even if primary fails");
    }

    #[test]
    fn delete_removes_both_copies() {
        let store = FallbackStore::new(MemoryStore::default(), MemoryStore::default());
        store.primary.set(KEY, &secret("a")).unwrap();
        store.fallback.set(KEY, &secret("b")).unwrap();
        store.delete(KEY).unwrap();
        assert!(store.get(KEY).unwrap().is_none());
    }
}
