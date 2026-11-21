//! Use cases and ports of the design context. Other contexts use it only through here.

pub mod ports;
pub mod review_design;

pub use crate::design::domain::{
    ALL_CHECKS, ComponentRef, DesignCheckKind, DesignSpec, FigmaLink, LinkError, TokenKind, TokenRef,
};
pub use ports::{
    CachedDesign, DesignCache, DesignError, FetchedDesign, FigmaSource, I18nSource, StatesJudge, StatesRequest,
};
pub use review_design::{DesignOutcome, DesignRequest, MAX_LINKS, NO_LINKS_NOTE, ReviewDesign};
