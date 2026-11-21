use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::review::application::{AcceptanceCriterion, Requirements, TicketContext};
use crate::tickets::domain::{ParentRef, Ticket, TicketKey, merge_links};

use super::ports::{CacheError, CachedTicket, TicketCache, TicketError, TicketSource};

const MAX_PARENT_HOPS: usize = 2;
pub const MAX_DESCRIPTION_CHARS: usize = 6000;
const TRUNCATION_MARKER: &str = " […truncated]";
const TITLE_SEPARATOR: &str = " | ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadOptions {
    pub include_parent_story: bool,
    pub include_description: bool,
    pub cache_ttl: Duration,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self { include_parent_story: true, include_description: true, cache_ttl: Duration::minutes(10) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedTickets {
    pub requirements: Requirements,
    pub context: TicketContext,
    pub notes: Vec<String>,
}

type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct LoadTicketContext<S> {
    source: S,
    cache: Option<Arc<dyn TicketCache>>,
    options: LoadOptions,
    clock: Clock,
}

impl<S: TicketSource + Sync> LoadTicketContext<S> {
    pub fn new(source: S, options: LoadOptions) -> Self {
        Self { source, cache: None, options, clock: Arc::new(Utc::now) }
    }

    pub fn with_cache(mut self, cache: Arc<dyn TicketCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    #[cfg(test)]
    fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub async fn execute(&self, keys: &[TicketKey]) -> Result<LoadedTickets, TicketError> {
        let mut notes = Vec::new();
        let mut implemented = Vec::new();
        let mut first_error = None;
        for key in keys {
            match self.load(key, &mut notes).await {
                Ok(ticket) => implemented.push(ticket),
                Err(error) => {
                    notes.push(format!("ticket {key} could not be loaded: {error}"));
                    first_error.get_or_insert(error);
                }
            }
        }
        if implemented.is_empty() {
            return Err(first_error.unwrap_or_else(|| TicketError::Invalid("no ticket key to load".into())));
        }
        let mut ancestors = Vec::new();
        if self.options.include_parent_story {
            for ticket in &implemented {
                self.walk_parents(ticket, &implemented, &mut ancestors, &mut notes).await;
            }
        }
        notes.dedup();
        Ok(self.assemble(&implemented, &ancestors, notes))
    }

    async fn walk_parents(
        &self,
        ticket: &Ticket,
        implemented: &[Ticket],
        ancestors: &mut Vec<Ticket>,
        notes: &mut Vec<String>,
    ) {
        if ticket.issue_type.is_none() && ticket.parent.is_none() {
            notes.push(format!(
                "parent Story of {} not loaded: the ticket source returned no issue type or parent",
                ticket.key
            ));
            return;
        }
        if ticket.is_story() {
            return;
        }
        let mut next: Option<ParentRef> = ticket.parent.clone();
        for _ in 0..MAX_PARENT_HOPS {
            let Some(parent) = next.take().filter(|p| !p.is_epic()) else { break };
            let is_loaded = |t: &Ticket| t.key == parent.key;
            if implemented.iter().any(is_loaded) || ancestors.iter().any(is_loaded) {
                break;
            }
            let loaded = match self.load(&parent.key, notes).await {
                Ok(loaded) => loaded,
                Err(error) => {
                    notes.push(format!("parent {} of {} could not be loaded: {error}", parent.key, ticket.key));
                    break;
                }
            };
            if loaded.is_epic() {
                break;
            }
            let is_story = loaded.is_story();
            next = loaded.parent.clone();
            ancestors.push(loaded);
            if is_story {
                break;
            }
        }
    }

    async fn load(&self, key: &TicketKey, notes: &mut Vec<String>) -> Result<Ticket, TicketError> {
        let Some(cache) = &self.cache else {
            return self.source.fetch(key).await;
        };
        let now = (self.clock)();
        let cached = cache.get_ticket(key).await.unwrap_or_else(|error| {
            notes.push(cache_note(&error));
            None
        });
        if let Some(entry) = cached {
            if now - entry.fetched_at < self.options.cache_ttl {
                return Ok(entry.ticket);
            }
            match self.source.fetch_updated(key).await {
                Ok(Some(updated)) if entry.jira_updated.as_ref() == Some(&updated) => {
                    let refreshed = CachedTicket { fetched_at: now, ..entry };
                    self.store(cache.as_ref(), &refreshed, notes).await;
                    return Ok(refreshed.ticket);
                }
                Err(error @ TicketError::Unavailable(_)) => return Ok(stale(entry, &error, notes)),
                _ => {}
            }
            return match self.source.fetch(key).await {
                Ok(ticket) => Ok(self.store_fresh(cache.as_ref(), ticket, now, notes).await),
                Err(error @ TicketError::Unavailable(_)) => Ok(stale(entry, &error, notes)),
                Err(error) => Err(error),
            };
        }
        let ticket = self.source.fetch(key).await?;
        Ok(self.store_fresh(cache.as_ref(), ticket, now, notes).await)
    }

    async fn store_fresh(
        &self,
        cache: &dyn TicketCache,
        ticket: Ticket,
        now: DateTime<Utc>,
        notes: &mut Vec<String>,
    ) -> Ticket {
        let entry = CachedTicket { jira_updated: ticket.updated.clone(), ticket, fetched_at: now };
        self.store(cache, &entry, notes).await;
        entry.ticket
    }

    async fn store(&self, cache: &dyn TicketCache, entry: &CachedTicket, notes: &mut Vec<String>) {
        if let Err(error) = cache.put_ticket(entry).await {
            notes.push(cache_note(&error));
        }
    }

    fn assemble(&self, implemented: &[Ticket], ancestors: &[Ticket], notes: Vec<String>) -> LoadedTickets {
        let all: Vec<&Ticket> = implemented.iter().chain(ancestors).collect();
        let main = &implemented[0];
        let title = if implemented.len() == 1 {
            main.title.clone()
        } else {
            implemented.iter().map(|t| format!("{}: {}", t.key, t.title)).collect::<Vec<_>>().join(TITLE_SEPARATOR)
        };
        let criteria = all
            .iter()
            .flat_map(|ticket| {
                ticket.criteria.iter().enumerate().map(|(index, text)| AcceptanceCriterion {
                    id: format!("{}/AC-{}", ticket.key, index + 1),
                    text: text.clone(),
                })
            })
            .collect();
        let description = if self.options.include_description { describe(&all) } else { String::new() };
        let mut design_links = Vec::new();
        for ticket in ancestors.iter().chain(implemented) {
            merge_links(&mut design_links, ticket.design_links.iter().cloned());
        }
        LoadedTickets {
            requirements: Requirements { key: main.key.to_string(), title, url: main.url.clone(), criteria },
            context: TicketContext { keys: all.iter().map(|t| t.key.to_string()).collect(), description, design_links },
            notes,
        }
    }
}

fn stale(entry: CachedTicket, error: &TicketError, notes: &mut Vec<String>) -> Ticket {
    notes.push(format!(
        "{}: {error}; using the copy cached at {}",
        entry.ticket.key,
        entry.fetched_at.format("%Y-%m-%d %H:%M UTC")
    ));
    entry.ticket
}

fn cache_note(error: &CacheError) -> String {
    format!("ticket cache unavailable ({error}); tickets were fetched from the tracker")
}

fn describe(tickets: &[&Ticket]) -> String {
    tickets
        .iter()
        .filter(|t| !t.description.trim().is_empty())
        .map(|t| format!("[{}] {}\n{}", t.key, t.title, truncate_chars(t.description.trim(), MAX_DESCRIPTION_CHARS)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}{TRUNCATION_MARKER}", &text[..cut]),
        None => text.to_string(),
    }
}

#[cfg(test)]
#[path = "load_ticket_context_tests.rs"]
mod tests;
