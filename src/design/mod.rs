//! Design bounded context: compares changed UI code with the Figma frames linked from the ticket.
//!
//! `domain` holds the design model and the pure, deterministic mismatch checks (copy, token, component);
//! `application` the `ReviewDesign` use case and its ports; `infrastructure` the Figma REST source, the
//! SQLite cache, the i18n file reader, the Claude states judge and the review-context adapter.

pub mod application;
pub mod domain;
pub mod infrastructure;
