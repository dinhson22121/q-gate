//! Ports implemented by tickets adapters: ticket trackers (Jira REST, Jira MCP), the SQLite cache
//! and the model that writes ticket briefs.

use std::future::Future;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;

use crate::review::application::agent_ports::AgentError;
use crate::review::application::{AcceptanceCriterion, TicketBrief};
use crate::tickets::domain::{Ticket, TicketKey};

/// Why a ticket could not be fetched. Messages never contain credentials.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TicketError {
    #[error("ticket {0} not found")]
    NotFound(String),
    #[error("not authorized to read the ticket: {0}")]
    Unauthorized(String),
    #[error("ticket tracker unavailable: {0}")]
    Unavailable(String),
    #[error("invalid response from ticket tracker: {0}")]
    Invalid(String),
    #[error("ticket tracker misconfigured: {0}")]
    Config(String),
}

/// Fetches a ticket and its acceptance criteria from a tracker.
pub trait TicketSource {
    fn fetch(&self, key: &TicketKey) -> impl Future<Output = Result<Ticket, TicketError>> + Send;

    /// Cheap freshness probe: the tracker's last-modified marker of the ticket (Jira `fields.updated`).
    /// `Ok(None)` when the tracker cannot tell, in which case callers re-fetch the whole ticket.
    fn fetch_updated(&self, _key: &TicketKey) -> impl Future<Output = Result<Option<String>, TicketError>> + Send {
        async { Ok(None) }
    }
}

/// Storage failure of a cache. Callers treat caches as best effort.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cache error: {0}")]
pub struct CacheError(pub String);

/// A ticket as stored in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTicket {
    pub ticket: Ticket,
    /// The tracker's last-modified marker at fetch time.
    pub jira_updated: Option<String>,
    /// When the ticket was last fetched or confirmed unchanged.
    pub fetched_at: DateTime<Utc>,
}

/// Cache of parsed tickets, keyed by ticket key.
pub trait TicketCache: Send + Sync {
    fn get_ticket<'a>(&'a self, key: &'a TicketKey) -> BoxFuture<'a, Result<Option<CachedTicket>, CacheError>>;
    /// Inserts or replaces the entry for `entry.ticket.key`.
    fn put_ticket<'a>(&'a self, entry: &'a CachedTicket) -> BoxFuture<'a, Result<(), CacheError>>;
}

/// Cache of ticket briefs, keyed by a content hash of everything the brief was derived from.
pub trait BriefCache: Send + Sync {
    fn get_brief<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, Result<Option<TicketBrief>, CacheError>>;
    fn put_brief<'a>(&'a self, hash: &'a str, brief: &'a TicketBrief) -> BoxFuture<'a, Result<(), CacheError>>;
}

/// Everything the brief generator may read. All text fields are untrusted data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefRequest {
    pub criteria: Vec<AcceptanceCriterion>,
    /// Ticket (and parent Story) descriptions, plain text.
    pub description: String,
    pub design_links: Vec<String>,
    /// Compact view of the change: file paths and diff hunks, already capped.
    pub change_view: String,
    pub max_scenarios: usize,
}

/// Writes a ticket brief (one model call). The result is validated by the caller.
pub trait BriefGenerator: Send + Sync {
    fn generate<'a>(&'a self, request: &'a BriefRequest) -> BoxFuture<'a, Result<TicketBrief, AgentError>>;
}
