use std::fmt;
use std::sync::Arc;

use crate::access::domain::constant_time_eq;

pub const TOKEN_HEADER: &str = "x-qgate-token";
pub const TOKEN_PLACEHOLDER: &str = "__QGATE_TOKEN__";
const TOKEN_BYTES: usize = 32;

#[derive(Clone)]
pub struct ApiToken(Arc<str>);

impl ApiToken {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)?;
        Ok(Self(hex(&bytes).into()))
    }

    pub fn verify(&self, presented: Option<&str>) -> bool {
        presented.is_some_and(|value| constant_time_eq(self.0.as_bytes(), value.as_bytes()))
    }

    pub fn inject(&self, html: &str) -> String {
        html.replace(TOKEN_PLACEHOLDER, &self.0)
    }

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
