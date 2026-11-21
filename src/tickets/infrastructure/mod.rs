//! Tickets adapters: Jira REST (default) and Jira MCP (optional), ADF conversion, the SQLite
//! ticket/brief cache and the Claude-backed Ticket Analyst.

pub mod adf;
pub mod any_source;
pub mod claude_analyst;
pub mod jira_mcp;
pub mod jira_rest;
pub mod secret;
pub mod sqlite_cache;

pub use any_source::AnyJiraSource;
pub use claude_analyst::{ClaudeBriefGenerator, ClaudeTicketAnalyst};
pub use jira_mcp::JiraMcpSource;
pub use jira_rest::{JiraRestConfig, JiraRestSource};
pub use secret::Secret;
pub use sqlite_cache::SqliteTicketCache;
