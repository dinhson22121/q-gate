//! Per-instance API token required on every mutating dashboard route.
//!
//! Why: the `X-Qgate-Request` header and `Origin` check stop browsers (cross-site pages), but not a local
//! process, which can send any header. The token is random per server start and only handed out inside
//! our own HTML pages. A cross-origin page cannot read those pages (same-origin policy), so it cannot
//! learn the token. A local process that can reach the dashboard CAN fetch a page and scrape the token:
//! the token raises the bar (no blind `curl`, a restart invalidates it) but is not a boundary against
//! local code by itself. Pages (and so the token) are only served to logged-in sessions (see `crate::access`),
//! which keeps out code that can reach the port without the login token. Also, the settings that matter most
//! (endpoints, commands, webhook, turning repository checks on) are not editable over HTTP at all; see `settings::domain::ADMIN_ONLY_FIELDS`.

use std::fmt;
use std::sync::Arc;

use crate::access::domain::constant_time_eq;

/// Header the pages send the token in.
pub const TOKEN_HEADER: &str = "x-qgate-token";
/// Replaced in the page HTML with the token (`<meta name="qgate-token" content="__QGATE_TOKEN__">`).
pub const TOKEN_PLACEHOLDER: &str = "__QGATE_TOKEN__";
const TOKEN_BYTES: usize = 32;

/// Hex token, generated once per process and kept only in memory. `Debug` never shows it.
#[derive(Clone)]
pub struct ApiToken(Arc<str>);

impl ApiToken {
    /// 32 bytes from the OS CSPRNG, hex-encoded.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)?;
        Ok(Self(hex(&bytes).into()))
    }

    /// Constant-time comparison with the presented header value; `None` (no header) never matches.
    pub fn verify(&self, presented: Option<&str>) -> bool {
        presented.is_some_and(|value| constant_time_eq(self.0.as_bytes(), value.as_bytes()))
    }

    /// `html` with every [`TOKEN_PLACEHOLDER`] replaced by the token (hex: safe inside an attribute).
    pub fn inject(&self, html: &str) -> String {
        html.replace(TOKEN_PLACEHOLDER, &self.0)
    }

    /// The raw value, for tests that act as the page.
    #[cfg(test)]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_distinct_hex_tokens() {
        let (a, b) = (ApiToken::generate().unwrap(), ApiToken::generate().unwrap());
        assert_eq!(a.expose().len(), TOKEN_BYTES * 2);
        assert!(a.expose().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.expose(), b.expose());
        assert_eq!(format!("{a:?}"), "ApiToken(<redacted>)");
    }

    #[test]
    fn verifies_only_the_exact_token() {
        let token = ApiToken::generate().unwrap();
        assert!(token.verify(Some(token.expose())));
        assert!(!token.verify(None));
        assert!(!token.verify(Some("")));
        assert!(!token.verify(Some(&token.expose()[1..])));
        let mut wrong = token.expose().to_string();
        wrong.replace_range(63..64, if wrong.ends_with('0') { "1" } else { "0" });
        assert!(!token.verify(Some(&wrong)));
    }

    #[test]
    fn injects_into_the_placeholder() {
        let token = ApiToken::generate().unwrap();
        let html = token.inject(r#"<meta name="qgate-token" content="__QGATE_TOKEN__">"#);
        assert_eq!(html, format!(r#"<meta name="qgate-token" content="{}">"#, token.expose()));
        assert!(constant_time_eq(b"abc", b"abc") && !constant_time_eq(b"abc", b"abd"));
    }
}
