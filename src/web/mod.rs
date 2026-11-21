//! Shared HTTP plumbing for the dashboard's api layers (settings, rulebook, policy): the per-instance
//! API token and the checks every mutating route runs. Used only by `*/api` modules; no domain code.

mod api_token;
mod mutation_guard;

pub use api_token::{ApiToken, TOKEN_HEADER, TOKEN_PLACEHOLDER};
pub use mutation_guard::{MutationGuard, REQUEST_HEADER, Rejection, normalize_host};
