use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

use super::base64url;

const REDACTED: &str = "***";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum JiraSiteError {
    #[error("site must be a valid URL such as https://yourcompany.atlassian.net")]
    Malformed,
    #[error("site must use https")]
    NotHttps,
    #[error("site must not contain credentials")]
    HasCredentials,
}

pub fn site_hostname(site: &str) -> Result<String, JiraSiteError> {
    let url = Url::parse(site.trim()).map_err(|_| JiraSiteError::Malformed)?;
    if url.scheme() != "https" {
        return Err(JiraSiteError::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(JiraSiteError::HasCredentials);
    }
    url.host_str().map(str::to_ascii_lowercase).ok_or(JiraSiteError::Malformed)
}

#[derive(Debug, thiserror::Error)]
#[error("stored Jira OAuth credentials are unreadable")]
pub struct BundleDecodeError;

#[derive(Clone, Serialize, Deserialize)]
pub struct TokenBundle {
    pub client_id: String,
    pub token_endpoint: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub site_hostname: String,
}

impl TokenBundle {
    pub fn is_expired(&self, now: DateTime<Utc>, skew: Duration) -> bool {
        now + skew >= self.expires_at
    }

    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        base64url::encode(&json)
    }

    pub fn decode(encoded: &str) -> Result<Self, BundleDecodeError> {
        let bytes = base64url::decode(encoded.trim()).ok_or(BundleDecodeError)?;
        serde_json::from_slice(&bytes).map_err(|_| BundleDecodeError)
    }
}

impl fmt::Debug for TokenBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenBundle")
            .field("client_id", &self.client_id)
            .field("token_endpoint", &self.token_endpoint)
            .field("access_token", &REDACTED)
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| REDACTED))
            .field("expires_at", &self.expires_at)
            .field("site_hostname", &self.site_hostname)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> TokenBundle {
        TokenBundle {
            client_id: "client-1".into(),
            token_endpoint: "https://auth.example/token".into(),
            access_token: "access-secret-value".into(),
            refresh_token: Some("refresh-secret-value".into()),
            expires_at: DateTime::from_timestamp(2_000_000_000, 0).unwrap(),
            site_hostname: "acme.atlassian.net".into(),
        }
    }

    #[test]
    fn encoded_bundle_round_trips_without_whitespace() {
        let encoded = bundle().encode();
        assert!(!encoded.chars().any(char::is_whitespace) && !encoded.contains('='));
        let decoded = TokenBundle::decode(&encoded).unwrap();
        assert_eq!(decoded.access_token, "access-secret-value");
        assert_eq!(decoded.refresh_token.as_deref(), Some("refresh-secret-value"));
        assert_eq!(decoded.expires_at, bundle().expires_at);
        assert_eq!(decoded.site_hostname, "acme.atlassian.net");
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(TokenBundle::decode("not base64 !!").is_err());
        assert!(TokenBundle::decode(&base64url::encode(b"{}")).is_err());
    }

    #[test]
    fn debug_never_prints_tokens() {
        let text = format!("{:?}", bundle());
        assert!(!text.contains("access-secret-value") && !text.contains("refresh-secret-value"), "{text}");
        assert!(text.contains("client-1"));
    }

    #[test]
    fn expiry_honours_the_skew() {
        let b = bundle();
        let before = b.expires_at - Duration::seconds(100);
        assert!(!b.is_expired(before, Duration::seconds(60)));
        assert!(b.is_expired(before, Duration::seconds(120)));
        assert!(b.is_expired(b.expires_at, Duration::zero()));
    }

    #[test]
    fn site_must_be_a_plain_https_url() {
        assert_eq!(site_hostname(" https://Acme.Atlassian.net/ ").unwrap(), "acme.atlassian.net");
        assert_eq!(site_hostname("http://acme.atlassian.net"), Err(JiraSiteError::NotHttps));
        assert_eq!(site_hostname("acme.atlassian.net"), Err(JiraSiteError::Malformed));
        assert_eq!(site_hostname("https://u:p@acme.atlassian.net"), Err(JiraSiteError::HasCredentials));
        assert_eq!(site_hostname(""), Err(JiraSiteError::Malformed));
    }
}
