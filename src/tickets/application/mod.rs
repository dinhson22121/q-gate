//! Tickets use cases and the ports they need. Other contexts use the tickets context only through here.

pub mod analyze_ticket;
pub mod fetch_requirements;
pub mod load_ticket_context;
pub mod ports;

pub use crate::tickets::domain::{
    ImplementedKeys, InvalidTicketKey, MAX_IMPLEMENTED_KEYS, Ticket, TicketKey, extract_implemented_keys,
    extract_ticket_key,
};
pub use analyze_ticket::{AnalyzeTicket, MAX_CHANGE_VIEW_CHARS};
pub use fetch_requirements::{FetchRequirements, to_requirements};
pub use load_ticket_context::{LoadOptions, LoadTicketContext, LoadedTickets};
pub use ports::{
    BriefCache, BriefGenerator, BriefRequest, CacheError, CachedTicket, TicketCache, TicketError, TicketSource,
};
