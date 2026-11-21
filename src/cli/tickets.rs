use crate::settings::application::{JiraConfig, JiraProvider, Secret};
use std::sync::Arc;

use crate::tickets::application::{
    LoadOptions, LoadTicketContext, LoadedTickets, Ticket, TicketCache, TicketError, TicketKey, TicketSource,
};
use crate::tickets::domain::{MAX_IMPLEMENTED_KEYS, extract_implemented_keys};
use crate::tickets::infrastructure::{JiraMcpSource, JiraRestConfig, JiraRestSource, Secret as TicketSecret};

pub enum AnyTicketSource {
    Rest(JiraRestSource),
    Mcp(JiraMcpSource),
}

impl TicketSource for AnyTicketSource {
    async fn fetch(&self, key: &TicketKey) -> Result<Ticket, TicketError> {
        match self {
            Self::Rest(source) => source.fetch(key).await,
            Self::Mcp(source) => source.fetch(key).await,
        }
    }
}

pub fn build_source(jira: &JiraConfig, token: Option<&Secret>) -> Result<AnyTicketSource, TicketError> {
    match jira.provider {
        JiraProvider::Rest => {
            let token = token.ok_or_else(|| {
                TicketError::Config("no Jira API token: set JIRA_API_TOKEN (or JIRA_API_TOKEN_FILE)".into())
            })?;
            let config = JiraRestConfig {
                base_url: jira.base_url.clone(),
                email: jira.email.clone(),
                token: TicketSecret::new(token.expose()),
                ac_field: Some(jira.ac_field.clone()).filter(|f| !f.is_empty()),
            };
            JiraRestSource::new(config).map(AnyTicketSource::Rest)
        }
        JiraProvider::Mcp => {
            let env = jira
                .mcp
                .env_passthrough
                .iter()
                .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
                .collect();
            JiraMcpSource::new(jira.mcp.command.clone(), jira.mcp.tool.clone(), env).map(AnyTicketSource::Mcp)
        }
    }
}

pub struct TicketLoad {
    pub loaded: Option<LoadedTickets>,
    pub notes: Vec<String>,
}

pub async fn load_ticket_context(
    jira: &JiraConfig,
    token: Option<&Secret>,
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
    let mut use_case = LoadTicketContext::new(build_source(jira, token)?, options);
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

    #[test]
    fn rest_provider_requires_a_token() {
        let jira = JiraConfig { enabled: true, ..JiraConfig::default() };
        assert!(matches!(build_source(&jira, None), Err(TicketError::Config(_))));
    }

    #[tokio::test]
    async fn disabled_jira_without_explicit_ticket_loads_nothing() {
        let messages = vec!["SHOP-1: add export".to_string()];
        let load = load_ticket_context(
            &JiraConfig::default(),
            None,
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
        let jira = JiraConfig { enabled: true, ..JiraConfig::default() };
        let load =
            load_ticket_context(&jira, None, None, Some("main"), &["tidy up".into()], LoadOptions::default(), None)
                .await
                .unwrap();
        assert!(load.notes.iter().any(|n| n.contains("no Jira ticket key")));
        let invalid =
            load_ticket_context(&jira, None, Some("not a key"), None, &[], LoadOptions::default(), None).await;
        assert!(matches!(invalid, Err(TicketError::Invalid(_))));
    }

    #[tokio::test]
    async fn references_are_noted_and_missing_token_is_a_config_error() {
        let jira = JiraConfig { enabled: true, ..JiraConfig::default() };
        let messages = vec!["SHOP-7: export\n\nsee PAY-12".to_string()];
        let result = load_ticket_context(&jira, None, None, None, &messages, LoadOptions::default(), None).await;
        assert!(matches!(result, Err(TicketError::Config(_))), "REST without a token cannot fetch");
    }
}
