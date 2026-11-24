use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;

use crate::settings::application::{CredentialError, CredentialName, CredentialStore, store_secret};
use crate::tickets::domain::{JiraSiteError, TokenBundle, site_hostname};

const PENDING_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum OauthError {
    #[error(transparent)]
    Site(#[from] JiraSiteError),
    #[error("Jira authorization server discovery failed: {0}")]
    Discovery(String),
    #[error("client registration failed: {0}")]
    Registration(String),
    #[error("token request failed: {0}")]
    Token(String),
    #[error("unknown or expired authorization attempt")]
    UnknownState,
    #[error("credential storage failed: {0}")]
    Storage(String),
}

impl From<CredentialError> for OauthError {
    fn from(error: CredentialError) -> Self {
        Self::Storage(error.to_string())
    }
}

#[derive(Clone)]
pub struct PendingAuthorization {
    pub state: String,
    pub code_verifier: String,
    pub client_id: String,
    pub token_endpoint: String,
    pub redirect_uri: String,
    pub site_hostname: String,
}

impl fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("client_id", &self.client_id)
            .field("token_endpoint", &self.token_endpoint)
            .field("redirect_uri", &self.redirect_uri)
            .field("site_hostname", &self.site_hostname)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Authorization {
    pub authorize_url: String,
    pub pending: PendingAuthorization,
}

pub trait JiraOauthGateway: Send + Sync {
    fn begin<'a>(
        &'a self,
        site_hostname: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Authorization, OauthError>>;

    fn exchange<'a>(
        &'a self,
        pending: &'a PendingAuthorization,
        code: &'a str,
    ) -> BoxFuture<'a, Result<TokenBundle, OauthError>>;

    fn refresh<'a>(&'a self, bundle: &'a TokenBundle) -> BoxFuture<'a, Result<TokenBundle, OauthError>>;
}

#[derive(Default)]
pub struct PendingAuthorizations {
    entries: Mutex<HashMap<String, (Instant, PendingAuthorization)>>,
}

impl PendingAuthorizations {
    pub fn insert(&self, pending: PendingAuthorization) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, (created, _)| created.elapsed() < PENDING_TTL);
        while entries.len() >= MAX_PENDING {
            let Some(oldest) = entries.iter().min_by_key(|(_, (created, _))| *created).map(|(k, _)| k.clone()) else {
                break;
            };
            entries.remove(&oldest);
        }
        entries.insert(pending.state.clone(), (Instant::now(), pending));
    }

    pub fn take(&self, state: &str) -> Option<PendingAuthorization> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let (created, pending) = entries.remove(state)?;
        (created.elapsed() < PENDING_TTL).then_some(pending)
    }
}

pub fn save_bundle<S>(bundle: &TokenBundle, store: &S) -> Result<(), CredentialError>
where
    S: CredentialStore + ?Sized,
{
    store_secret(CredentialName::JiraOauth, &bundle.encode(), store)
}

pub fn load_bundle<S>(store: &S) -> Result<Option<TokenBundle>, CredentialError>
where
    S: CredentialStore + ?Sized,
{
    Ok(store.get(CredentialName::JiraOauth)?.and_then(|secret| TokenBundle::decode(secret.expose()).ok()))
}

#[derive(Debug, PartialEq, Eq)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub site: Option<String>,
}

pub struct JiraOauthService<G: ?Sized> {
    gateway: std::sync::Arc<G>,
    pending: PendingAuthorizations,
}

impl<G: JiraOauthGateway + ?Sized> JiraOauthService<G> {
    pub fn new(gateway: std::sync::Arc<G>) -> Self {
        Self { gateway, pending: PendingAuthorizations::default() }
    }

    pub async fn connect(&self, site: &str, redirect_uri: &str) -> Result<String, OauthError> {
        let hostname = site_hostname(site)?;
        let authorization = self.gateway.begin(&hostname, redirect_uri).await?;
        self.pending.insert(authorization.pending);
        Ok(authorization.authorize_url)
    }

    pub async fn complete<S>(&self, state: &str, code: &str, store: &S) -> Result<(), OauthError>
    where
        S: CredentialStore + ?Sized,
    {
        let pending = self.pending.take(state).ok_or(OauthError::UnknownState)?;
        let bundle = self.gateway.exchange(&pending, code).await?;
        save_bundle(&bundle, store)?;
        Ok(())
    }

    pub fn status<S>(&self, store: &S) -> Result<ConnectionStatus, OauthError>
    where
        S: CredentialStore + ?Sized,
    {
        let bundle = load_bundle(store)?;
        Ok(ConnectionStatus { connected: bundle.is_some(), site: bundle.map(|b| b.site_hostname) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, Utc};

    use super::*;
    use crate::settings::application::credentials::fakes::MemoryStore;

    struct FakeGateway;

    fn pending(state: &str) -> PendingAuthorization {
        PendingAuthorization {
            state: state.into(),
            code_verifier: "verifier-secret".into(),
            client_id: "cid".into(),
            token_endpoint: "https://auth.example/token".into(),
            redirect_uri: "http://localhost/cb".into(),
            site_hostname: "acme.atlassian.net".into(),
        }
    }

    fn bundle() -> TokenBundle {
        TokenBundle {
            client_id: "cid".into(),
            token_endpoint: "https://auth.example/token".into(),
            access_token: "a".into(),
            refresh_token: None,
            expires_at: DateTime::<Utc>::from_timestamp(2_000_000_000, 0).unwrap(),
            site_hostname: "acme.atlassian.net".into(),
        }
    }

    impl JiraOauthGateway for FakeGateway {
        fn begin<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<Authorization, OauthError>> {
            Box::pin(async {
                Ok(Authorization { authorize_url: "https://auth.example/authorize".into(), pending: pending("s1") })
            })
        }

        fn exchange<'a>(
            &'a self,
            _: &'a PendingAuthorization,
            code: &'a str,
        ) -> BoxFuture<'a, Result<TokenBundle, OauthError>> {
            Box::pin(
                async move { if code == "bad" { Err(OauthError::Token("rejected".into())) } else { Ok(bundle()) } },
            )
        }

        fn refresh<'a>(&'a self, b: &'a TokenBundle) -> BoxFuture<'a, Result<TokenBundle, OauthError>> {
            Box::pin(async move { Ok(b.clone()) })
        }
    }

    fn service() -> JiraOauthService<FakeGateway> {
        JiraOauthService::new(Arc::new(FakeGateway))
    }

    #[tokio::test]
    async fn connect_then_complete_persists_the_bundle() {
        let store = MemoryStore::default();
        let service = service();
        assert_eq!(service.status(&store).unwrap(), ConnectionStatus { connected: false, site: None });
        let url = service.connect("https://acme.atlassian.net", "http://localhost/cb").await.unwrap();
        assert_eq!(url, "https://auth.example/authorize");
        service.complete("s1", "good", &store).await.unwrap();
        assert_eq!(
            service.status(&store).unwrap(),
            ConnectionStatus { connected: true, site: Some("acme.atlassian.net".into()) }
        );
        assert_eq!(load_bundle(&store).unwrap().unwrap().access_token, "a");
    }

    #[tokio::test]
    async fn complete_rejects_unknown_state_and_consumes_it_once() {
        let store = MemoryStore::default();
        let service = service();
        assert!(matches!(service.complete("nope", "c", &store).await, Err(OauthError::UnknownState)));
        service.connect("https://acme.atlassian.net", "http://localhost/cb").await.unwrap();
        assert!(matches!(service.complete("s1", "bad", &store).await, Err(OauthError::Token(_))));
        assert!(matches!(service.complete("s1", "good", &store).await, Err(OauthError::UnknownState)));
        assert!(!service.status(&store).unwrap().connected);
    }

    #[tokio::test]
    async fn connect_rejects_a_bad_site() {
        let result = service().connect("http://acme.atlassian.net", "http://localhost/cb").await;
        assert!(matches!(result, Err(OauthError::Site(JiraSiteError::NotHttps))));
    }

    #[test]
    fn pending_store_is_bounded_and_debug_hides_the_verifier() {
        let pending_store = PendingAuthorizations::default();
        for i in 0..(MAX_PENDING + 5) {
            pending_store.insert(pending(&format!("s{i}")));
        }
        assert!(pending_store.take("s0").is_none());
        assert!(pending_store.take(&format!("s{}", MAX_PENDING + 4)).is_some());
        assert!(!format!("{:?}", pending("x")).contains("verifier-secret"));
    }

    #[test]
    fn undecodable_stored_bundle_reads_as_not_connected() {
        let store = MemoryStore::default();
        store_secret(CredentialName::JiraOauth, "garbage", &store).unwrap();
        assert!(load_bundle(&store).unwrap().is_none());
    }
}
