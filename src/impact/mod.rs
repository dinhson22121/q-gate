//! Impact context: regression detection by re-running the repository's EXISTING tests for the
//! packages a change can affect, on the base commit and on the head commit.
//!
//! v1 is Go only and package-level: the impacted set is the changed packages plus everything that
//! (transitively) imports them. Java is deferred: dependency injection hides the real call edges,
//! so static package impact would be unreliable there.

pub mod application;
pub mod domain;
pub mod infrastructure;
