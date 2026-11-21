//! Access context: who may use the dashboard at all. One secret per installation (env or a 0600 file in
//! the data directory) is exchanged once, via a login link, for a long-lived HttpOnly session cookie that
//! every dashboard route requires. Code that can reach the port but not the secret (the isolated runner
//! container, other local processes) gets neither pages, data nor the per-page API token.

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
