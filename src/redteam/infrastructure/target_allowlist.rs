use crate::redteam::application::{RedteamError, TargetPolicy};
use crate::web::normalize_host;

pub struct AllowlistTargetPolicy {
    allowed: Vec<String>,
    isolated: Vec<String>,
}

impl AllowlistTargetPolicy {
    pub fn new(allowed_targets: &[String], isolated_targets: &[String]) -> Self {
        Self {
            allowed: allowed_targets.iter().map(|h| normalize_host(h)).filter(|h| !h.is_empty()).collect(),
            isolated: isolated_targets.iter().map(|h| normalize_host(h)).filter(|h| !h.is_empty()).collect(),
        }
    }
}

impl TargetPolicy for AllowlistTargetPolicy {
    fn check_host(&self, host: &str) -> Result<(), RedteamError> {
        let host = normalize_host(host);
        if self.allowed.contains(&host) { Ok(()) } else { Err(RedteamError::HostNotAllowed(host)) }
    }

    fn allows_destructive(&self, host: &str) -> bool {
        self.isolated.contains(&normalize_host(host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> AllowlistTargetPolicy {
        AllowlistTargetPolicy::new(&["127.0.0.1".into(), "Staging.Internal".into()], &["staging.internal".into()])
    }

    #[test]
    fn only_listed_hosts_are_allowed() {
        let policy = policy();
        assert!(policy.check_host("127.0.0.1").is_ok());
        assert!(policy.check_host("staging.internal").is_ok(), "host comparison is case-insensitive");
        assert_eq!(policy.check_host("evil.example"), Err(RedteamError::HostNotAllowed("evil.example".into())));
    }

    #[test]
    fn destructive_payloads_are_only_allowed_on_isolated_hosts() {
        let policy = policy();
        assert!(policy.allows_destructive("staging.internal"));
        assert!(!policy.allows_destructive("127.0.0.1"));
    }
}
