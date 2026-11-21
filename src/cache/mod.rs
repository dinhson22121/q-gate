//! Cache context: content-addressed storage of expensive results, reused only when every input
//! that produced them is unchanged.
//!
//! Wave 1 provides the foundation (the [`ContentKey`](domain::ContentKey) value type, the
//! [`ResultCache`](application::ResultCache) port and the
//! [`SqliteResultCache`](infrastructure::SqliteResultCache) adapter); it is not yet wired into the
//! review or testing contexts.

pub mod application;
pub mod domain;
pub mod infrastructure;
