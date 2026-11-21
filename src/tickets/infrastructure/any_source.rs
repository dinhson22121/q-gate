//! The configured Jira provider as one [`TicketSource`], forwarding the freshness probe too.

use crate::tickets::application::{TicketError, TicketSource};
use crate::tickets::domain::{Ticket, TicketKey};

use super::{JiraMcpSource, JiraRestSource};

pub enum AnyJiraSource {
    Rest(JiraRestSource),
    Mcp(JiraMcpSource),
}

impl TicketSource for AnyJiraSource {
    async fn fetch(&self, key: &TicketKey) -> Result<Ticket, TicketError> {
        match self {
            Self::Rest(source) => source.fetch(key).await,
            Self::Mcp(source) => source.fetch(key).await,
        }
    }

    async fn fetch_updated(&self, key: &TicketKey) -> Result<Option<String>, TicketError> {
        match self {
            Self::Rest(source) => source.fetch_updated(key).await,
            Self::Mcp(source) => source.fetch_updated(key).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mcp_has_no_freshness_probe_and_errors_are_forwarded() {
        let mcp = JiraMcpSource::new(vec!["/nonexistent/qgate-mcp".into()], "t".into(), vec![]).unwrap();
        let source = AnyJiraSource::Mcp(mcp);
        let key = TicketKey::parse("AB-1").unwrap();
        assert_eq!(source.fetch_updated(&key).await, Ok(None));
        assert!(matches!(source.fetch(&key).await, Err(TicketError::Config(_))));

        let rest = JiraRestSource::new(crate::tickets::infrastructure::JiraRestConfig {
            base_url: "http://127.0.0.1:9".into(),
            email: "a@b.c".into(),
            token: crate::tickets::infrastructure::Secret::new("t"),
            ac_field: None,
        })
        .unwrap();
        let source = AnyJiraSource::Rest(rest);
        assert!(matches!(source.fetch(&key).await, Err(TicketError::Unavailable(_))));
        assert!(matches!(source.fetch_updated(&key).await, Err(TicketError::Unavailable(_))));
    }
}
