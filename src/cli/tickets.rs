use crate::settings::application::{CredentialStore, JiraConfig};
use std::sync::Arc;
use std::time::Duration;

use crate::tickets::application::{LoadOptions, LoadTicketContext, LoadedTickets, TicketCache, TicketError, TicketKey};
use crate::tickets::domain::{MAX_IMPLEMENTED_KEYS, extract_implemented_keys};
use crate::tickets::infrastructure::{ATLASSIAN_MCP_URL, JiraMcpHttpSource, JiraOauthClient};

const MCP_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub fn build_source(_jira: &JiraConfig, store: &Arc<dyn CredentialStore>) -> Result<JiraMcpHttpSource, TicketError> {
    let http = reqwest::Client::builder()
        .timeout(MCP_HTTP_TIMEOUT)
        .build()
        .map_err(|e| TicketError::Config(format!("cannot build the Jira MCP HTTP client: {e}")))?;
    let gateway = Arc::new(JiraOauthClient::atlassian());
    Ok(JiraMcpHttpSource::new(http, ATLASSIAN_MCP_URL, Arc::clone(store), gateway))
}

pub struct TicketLoad {
    pub loaded: Option<LoadedTickets>,
    pub notes: Vec<String>,
}

pub async fn load_ticket_context(
    jira: &JiraConfig,
    store: &Arc<dyn CredentialStore>,
    explicit_key: Option<&str>,
    branch: Option<&str>,
    messages: &[String],
    options: LoadOptions,
    cache: Option<Arc<dyn TicketCache>>,
) -> Result<TicketLoad, TicketError> {
    let mut notes = Vec::new();
    let keys = match explicit_key {
        Some(raw) => vec![TicketKey::parse(raw).map_err(|e| TicketError::Invalid(e.to_string()))?],
        None if !jira.enabled => return Ok(TicketLoad { loaded: None, notes }),
        None => {
            let found = extract_implemented_keys(branch, messages, &jira.key_pattern_projects);
            if found.capped {
                notes.push(format!("only the first {MAX_IMPLEMENTED_KEYS} tickets are checked"));
            }
            if !found.references.is_empty() {
                let refs: Vec<String> = found.references.iter().map(ToString::to_string).collect();
                notes.push(format!("mentioned but not implemented (not checked): {}", refs.join(", ")));
            }
            found.implements
        }
    };
    if keys.is_empty() {
        notes.push("no Jira ticket key found in the branch name or commit messages".into());
        return Ok(TicketLoad { loaded: None, notes });
    }
    let mut use_case = LoadTicketContext::new(build_source(jira, store)?, options);
    if let Some(cache) = cache {
        use_case = use_case.with_cache(cache);
    }
    let loaded = use_case.execute(&keys).await?;
    notes.extend(loaded.notes.iter().cloned());
    Ok(TicketLoad { loaded: Some(loaded), notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::application::credentials::fakes::MemoryStore;
    use crate::tickets::application::TicketSource;

    fn memory_store() -> Arc<dyn CredentialStore> {
        Arc::new(MemoryStore::default())
    }

    #[tokio::test]
    async fn mcp_oauth_provider_builds_and_reports_not_connected() {
        let store = memory_store();
        let jira = JiraConfig { enabled: true, ..JiraConfig::default() };
        let source = build_source(&jira, &store).unwrap();
        let key = TicketKey::parse("KAN-1").unwrap();
        assert!(matches!(source.fetch(&key).await, Err(TicketError::Config(ref m)) if m.contains("not connected")));
    }

    #[tokio::test]
    async fn disabled_jira_without_explicit_ticket_loads_nothing() {
        let store = memory_store();
        let messages = vec!["SHOP-1: add export".to_string()];
        let load = load_ticket_context(
            &JiraConfig::default(),
            &store,
            None,
            Some("main"),
            &messages,
            LoadOptions::default(),
            None,
        )
        .await
        .unwrap();
        assert!(load.loaded.is_none());
    }

    #[tokio::test]
    async fn no_key_and_invalid_explicit_key_are_reported() {
        let store = memory_store();
        let jira = JiraConfig { enabled: true, ..JiraConfig::default() };
        let load =
            load_ticket_context(&jira, &store, None, Some("main"), &["tidy up".into()], LoadOptions::default(), None)
                .await
                .unwrap();
        assert!(load.notes.iter().any(|n| n.contains("no Jira ticket key")));
        let invalid =
            load_ticket_context(&jira, &store, Some("not a key"), None, &[], LoadOptions::default(), None).await;
        assert!(matches!(invalid, Err(TicketError::Invalid(_))));
    }

    #[tokio::test]
    async fn references_are_noted_and_missing_token_is_a_config_error() {
        let store = memory_store();
        let jira = JiraConfig { enabled: true, ..JiraConfig::default() };
        let messages = vec!["SHOP-7: export\n\nsee PAY-12".to_string()];
        let result = load_ticket_context(&jira, &store, None, None, &messages, LoadOptions::default(), None).await;
        assert!(matches!(result, Err(TicketError::Config(_))), "REST without a token cannot fetch");
    }
}
