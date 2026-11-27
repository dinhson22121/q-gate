pub mod adf;
pub mod claude_analyst;
pub mod claude_criteria;
pub mod jira_mcp;
pub mod jira_mcp_http;
pub mod jira_oauth;
pub mod secret;
pub mod sqlite_cache;

pub use claude_analyst::{ClaudeBriefGenerator, ClaudeTicketAnalyst};
pub use claude_criteria::ClaudeCriteriaInferrer;

pub fn prompt_material() -> Vec<String> {
    let mut material = claude_analyst::prompt_material();
    material.extend(claude_criteria::prompt_material());
    material
}
pub use jira_mcp_http::JiraMcpHttpSource;
pub use jira_oauth::{ATLASSIAN_MCP_URL, JiraOauthClient};
pub use secret::Secret;
pub use sqlite_cache::SqliteTicketCache;
