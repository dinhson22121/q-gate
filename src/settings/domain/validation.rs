use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self { field: field.into(), message: message.into() }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

pub type ValidationResult = Result<(), ValidationError>;

const HTTPS_SCHEME: &str = "https://";
const HTTP_SCHEME: &str = "http://";
const LOOPBACK_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

pub fn require_non_empty(field: &str, value: &str) -> ValidationResult {
    if value.trim().is_empty() {
        return Err(ValidationError::new(field, "must not be empty"));
    }
    Ok(())
}

pub fn require_range<T>(field: &str, value: T, min: T, max: T) -> ValidationResult
where
    T: PartialOrd + fmt::Display + Copy,
{
    if value < min || value > max {
        return Err(ValidationError::new(field, format!("must be between {min} and {max}, got {value}")));
    }
    Ok(())
}

pub fn require_secure_url(field: &str, url: &str) -> ValidationResult {
    if let Some(rest) = url.strip_prefix(HTTPS_SCHEME) {
        return require_host(field, rest);
    }
    if let Some(rest) = url.strip_prefix(HTTP_SCHEME) {
        require_host(field, rest)?;
        let host = host_of(rest);
        if LOOPBACK_HOSTS.contains(&host) {
            return Ok(());
        }
        return Err(ValidationError::new(
            field,
            format!("must use https:// (plain http is only allowed for localhost or 127.0.0.1), got '{url}'"),
        ));
    }
    Err(ValidationError::new(field, format!("must be an https:// URL, got '{url}'")))
}

pub fn require_http_url(field: &str, url: &str) -> ValidationResult {
    let rest = url
        .strip_prefix(HTTPS_SCHEME)
        .or_else(|| url.strip_prefix(HTTP_SCHEME))
        .ok_or_else(|| ValidationError::new(field, format!("must be an http:// or https:// URL, got '{url}'")))?;
    require_host(field, rest)
}

fn require_host(field: &str, after_scheme: &str) -> ValidationResult {
    let host = host_of(after_scheme);
    if host.is_empty() || host.chars().any(char::is_whitespace) || authority_of(after_scheme).contains('@') {
        return Err(ValidationError::new(field, "must contain a valid host (credentials in the URL are not allowed)"));
    }
    Ok(())
}

fn authority_of(after_scheme: &str) -> &str {
    after_scheme.split(['/', '?', '#']).next().unwrap_or_default()
}

fn host_of(after_scheme: &str) -> &str {
    authority_of(after_scheme).split(':').next().unwrap_or_default()
}

pub fn require_env_name(field: &str, name: &str) -> ValidationResult {
    let mut chars = name.chars();
    let valid_start = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !valid_start || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ValidationError::new(field, format!("'{name}' is not a valid environment variable name")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn plain_http_with_userinfo_is_not_loopback() {
        assert!(require_secure_url("llm.base_url", "http://localhost:x@evil.example/").is_err());
        assert!(require_secure_url("llm.base_url", "http://localhost@evil.example/").is_err());
        assert!(require_secure_url("llm.base_url", "http://localhost:8080/v1").is_ok());
    }
    use super::*;

    #[test]
    fn secure_url_accepts_https_and_loopback_http_only() {
        assert!(require_secure_url("f", "https://api.anthropic.com").is_ok());
        assert!(require_secure_url("f", "https://proxy.internal:8443/v1").is_ok());
        assert!(require_secure_url("f", "http://localhost:4000").is_ok());
        assert!(require_secure_url("f", "http://127.0.0.1/x").is_ok());
        assert!(require_secure_url("f", "http://localhost").is_ok());
        assert!(require_secure_url("f", "http://localhost.evil.com").is_err());
        assert!(require_secure_url("f", "http://example.com").is_err());
        assert!(require_secure_url("f", "ftp://example.com").is_err());
        assert!(require_secure_url("f", "https://").is_err());
        assert!(require_secure_url("f", "https://user:pw@example.com").is_err());
    }

    #[test]
    fn http_url_accepts_both_schemes() {
        assert!(require_http_url("f", "http://ntfy:80/topic").is_ok());
        assert!(require_http_url("f", "https://hooks.slack.com/x").is_ok());
        assert!(require_http_url("f", "hooks.slack.com").is_err());
        assert!(require_http_url("f", "http:///path").is_err());
        assert!(require_http_url("f", "https://hooks.example.com/x?to=a@b.c").is_ok());
    }

    #[test]
    fn range_and_empty_rules_name_the_field() {
        let err = require_range("llm.concurrency", 0, 1, 16).unwrap_err();
        assert_eq!(err.to_string(), "llm.concurrency: must be between 1 and 16, got 0");
        assert!(require_range("x", 16, 1, 16).is_ok());
        assert_eq!(require_non_empty("llm.model", "  ").unwrap_err().field, "llm.model");
        assert!(require_non_empty("llm.model", "m").is_ok());
    }

    #[test]
    fn env_names_must_be_portable() {
        assert!(require_env_name("f", "JIRA_URL").is_ok());
        assert!(require_env_name("f", "_X1").is_ok());
        assert!(require_env_name("f", "1X").is_err());
        assert!(require_env_name("f", "A-B").is_err());
        assert!(require_env_name("f", "").is_err());
    }
}
