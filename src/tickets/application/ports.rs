use std::future::Future;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;

use crate::review::application::agent_ports::AgentError;
use crate::review::application::{AcceptanceCriterion, TicketBrief};
use crate::tickets::domain::{Ticket, TicketKey};

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

pub trait TicketSource {
    fn fetch(&self, key: &TicketKey) -> impl Future<Output = Result<Ticket, TicketError>> + Send;

    fn fetch_updated(&self, _key: &TicketKey) -> impl Future<Output = Result<Option<String>, TicketError>> + Send {
        async { Ok(None) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cache error: {0}")]
pub struct CacheError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTicket {
    pub ticket: Ticket,
    pub jira_updated: Option<String>,
    pub fetched_at: DateTime<Utc>,
}

pub trait TicketCache: Send + Sync {
    fn get_ticket<'a>(&'a self, key: &'a TicketKey) -> BoxFuture<'a, Result<Option<CachedTicket>, CacheError>>;
    fn put_ticket<'a>(&'a self, entry: &'a CachedTicket) -> BoxFuture<'a, Result<(), CacheError>>;
}

pub trait BriefCache: Send + Sync {
    fn get_brief<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, Result<Option<TicketBrief>, CacheError>>;
    fn put_brief<'a>(&'a self, hash: &'a str, brief: &'a TicketBrief) -> BoxFuture<'a, Result<(), CacheError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefRequest {
    pub criteria: Vec<AcceptanceCriterion>,
    pub description: String,
    pub design_links: Vec<String>,
    pub change_view: String,
    pub callers: String,
    pub max_scenarios: usize,
}

pub trait BriefGenerator: Send + Sync {
    fn generate<'a>(&'a self, request: &'a BriefRequest) -> BoxFuture<'a, Result<TicketBrief, AgentError>>;
}

pub trait CriteriaInferrer: Send + Sync {
    fn infer<'a>(&'a self, title: &'a str, description: &'a str) -> BoxFuture<'a, Result<Vec<String>, AgentError>>;
}

pub trait CriteriaCache: Send + Sync {
    fn get_criteria<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, Result<Option<Vec<String>>, CacheError>>;
    fn put_criteria<'a>(&'a self, hash: &'a str, criteria: &'a [String]) -> BoxFuture<'a, Result<(), CacheError>>;
}
