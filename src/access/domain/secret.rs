use std::fmt;
use std::sync::Arc;

use sha2::{Digest, Sha256};

/// Random bytes in a generated secret (hex-encoded: 64 characters).
pub const SECRET_BYTES: usize = 32;
/// Shortest secret accepted from `QGATE_DASHBOARD_TOKEN` or the token file.
pub const MIN_SECRET_LEN: usize = 32;
/// Domain separation: the session cookie value is never the login secret itself.
const SESSION_DERIVATION_TAG: &[u8] = b"qgate-dashboard-session-v1\0";

/// Why a configured secret was refused. Never contains the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidSecret {
    #[error("must be at least {MIN_SECRET_LEN} characters (generate one with `openssl rand -hex 32`)")]
    TooShort,
    #[error("may only contain letters, digits, '-', '_', '.' and '~'")]
    BadCharacters,
}

/// The installation's dashboard login secret. Only URL-safe characters, so it can travel in a query string
/// without encoding. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct DashboardSecret(Arc<str>);

impl DashboardSecret {
    /// Validates a configured value (surrounding whitespace ignored).
    pub fn parse(raw: &str) -> Result<Self, InvalidSecret> {
        let value = raw.trim();
        if value.len() < MIN_SECRET_LEN {
            return Err(InvalidSecret::TooShort);
        }
        if !value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~')) {
            return Err(InvalidSecret::BadCharacters);
        }
        Ok(Self(value.into()))
    }

    /// A new secret from CSPRNG output (hex-encoded).
    pub fn from_random(bytes: &[u8; SECRET_BYTES]) -> Self {
        Self(hex(bytes).into())
    }

    /// Constant-time comparison with a presented login token.
    pub fn verify(&self, presented: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), presented.as_bytes())
    }

    /// The value of the session cookie: SHA-256 over a fixed tag and the secret. Stable across restarts
    /// (sessions survive them) and changes with the secret (rotating the secret logs everyone out).
    pub fn session_key(&self) -> SessionKey {
        let digest = Sha256::new().chain_update(SESSION_DERIVATION_TAG).chain_update(self.0.as_bytes()).finalize();
        SessionKey(hex(&digest).into())
    }

    /// The raw value: for the login link and the token file only. Never log it anywhere else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DashboardSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DashboardSecret(<redacted>)")
    }
}

/// Value of the session cookie. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionKey(Arc<str>);

impl SessionKey {
    /// Constant-time comparison with a presented cookie value.
    pub fn verify(&self, presented: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), presented.as_bytes())
    }

    /// The raw value, for the `Set-Cookie` header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionKey(<redacted>)")
    }
}

/// Runs in time independent of where the inputs differ. Lengths are not secret.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_trimmed_url_safe_values_of_minimum_length() {
        assert_eq!(DashboardSecret::parse(&format!("  {VALID}\n")).unwrap().expose(), VALID);
        assert!(DashboardSecret::parse("A-b_c.d~0123456789012345678901234").is_ok());
        assert_eq!(DashboardSecret::parse(&VALID[1..]), Err(InvalidSecret::TooShort));
        assert_eq!(DashboardSecret::parse(""), Err(InvalidSecret::TooShort));
        assert_eq!(DashboardSecret::parse(&format!("{VALID}&x=1")), Err(InvalidSecret::BadCharacters));
        assert_eq!(DashboardSecret::parse(&format!("{VALID} tail")), Err(InvalidSecret::BadCharacters));
    }

    #[test]
    fn errors_and_debug_never_show_the_value() {
        let secret = DashboardSecret::parse(VALID).unwrap();
        assert_eq!(format!("{secret:?}"), "DashboardSecret(<redacted>)");
        assert_eq!(format!("{:?}", secret.session_key()), "SessionKey(<redacted>)");
        let message = DashboardSecret::parse("short-secret-value").unwrap_err().to_string();
        assert!(!message.contains("short-secret-value"));
    }

    #[test]
    fn random_secrets_are_64_hex_characters() {
        let secret = DashboardSecret::from_random(&[0xab; SECRET_BYTES]);
        assert_eq!(secret.expose(), "ab".repeat(SECRET_BYTES));
        assert!(DashboardSecret::parse(secret.expose()).is_ok());
    }

    #[test]
    fn verifies_only_the_exact_secret() {
        let secret = DashboardSecret::parse(VALID).unwrap();
        assert!(secret.verify(VALID));
        assert!(!secret.verify(""));
        assert!(!secret.verify(&VALID[1..]));
        assert!(!secret.verify(&format!("{}0", &VALID[..31])));
    }

    #[test]
    fn session_key_is_stable_distinct_from_the_secret_and_tied_to_it() {
        let secret = DashboardSecret::parse(VALID).unwrap();
        let key = secret.session_key();
        assert_eq!(key, DashboardSecret::parse(VALID).unwrap().session_key());
        assert_eq!(key.expose().len(), 64);
        assert_ne!(key.expose(), VALID);
        assert!(key.verify(key.expose()));
        assert!(!key.verify(VALID));
        let other = DashboardSecret::from_random(&[1; SECRET_BYTES]).session_key();
        assert_ne!(key, other);
    }

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
