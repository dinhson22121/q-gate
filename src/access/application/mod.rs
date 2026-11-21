use crate::access::domain::{DashboardSecret, InvalidSecret};

pub const ENV_DASHBOARD_TOKEN: &str = "QGATE_DASHBOARD_TOKEN";

#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("{ENV_DASHBOARD_TOKEN} is invalid: {0}")]
    InvalidEnv(InvalidSecret),
    #[error("{0}")]
    Storage(String),
}

pub trait SecretStore: Send + Sync {
    fn load_or_create(&self) -> Result<DashboardSecret, AccessError>;
}

pub struct ResolveDashboardSecret<'a> {
    store: &'a dyn SecretStore,
}

impl<'a> ResolveDashboardSecret<'a> {
    pub fn new(store: &'a dyn SecretStore) -> Self {
        Self { store }
    }

    pub fn execute(&self, env_value: Option<&str>) -> Result<DashboardSecret, AccessError> {
        match env_value.map(str::trim).filter(|v| !v.is_empty()) {
            Some(value) => DashboardSecret::parse(value).map_err(AccessError::InvalidEnv),
            None => self.store.load_or_create(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::access::domain::SECRET_BYTES;

    #[derive(Default)]
    struct CountingStore(AtomicUsize);

    impl SecretStore for CountingStore {
        fn load_or_create(&self) -> Result<DashboardSecret, AccessError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(DashboardSecret::from_random(&[7; SECRET_BYTES]))
        }
    }

    const ENV_SECRET: &str = "env-secret-0123456789-0123456789-abc";

    #[test]
    fn env_value_wins_and_skips_the_store() {
        let store = CountingStore::default();
        let secret = ResolveDashboardSecret::new(&store).execute(Some(ENV_SECRET)).unwrap();
        assert_eq!(secret.expose(), ENV_SECRET);
        assert_eq!(store.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn blank_or_missing_env_uses_the_store() {
        let store = CountingStore::default();
        let use_case = ResolveDashboardSecret::new(&store);
        assert_eq!(use_case.execute(None).unwrap().expose(), "07".repeat(SECRET_BYTES));
        assert!(use_case.execute(Some("  ")).is_ok());
        assert_eq!(store.0.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn invalid_env_value_fails_without_echoing_it() {
        let store = CountingStore::default();
        let err = ResolveDashboardSecret::new(&store).execute(Some("too-short-secret")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains(ENV_DASHBOARD_TOKEN), "{message}");
        assert!(!message.contains("too-short-secret"), "{message}");
        assert_eq!(store.0.load(Ordering::SeqCst), 0);
    }
}
