//! Rulebook page and JSON API. Merged into the dashboard router (`history::api::router_with`) so its
//! Host check and security headers apply; mutating routes add CSRF and API-token checks ([`guard`]).

mod guard;
mod handlers;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};

use crate::conventions::application::RulebookService;
use crate::web::{ApiToken, MutationGuard};

pub use guard::{REQUEST_HEADER, REQUEST_HEADER_VALUE};

/// Request bodies above this are rejected with 413.
pub const MAX_BODY_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct RulebookApiState {
    service: Arc<RulebookService>,
    guard: Arc<MutationGuard>,
}

impl RulebookApiState {
    /// `allowed_hosts`: the dashboard's `dashboard.allowed_hosts`; an `Origin` header must name one of them.
    /// `token`: the per-instance API token, injected into the page and required on mutating routes.
    pub fn new(service: Arc<RulebookService>, allowed_hosts: Vec<String>, token: ApiToken) -> Self {
        let guard = Arc::new(MutationGuard::new(guard::REQUEST_HEADER_VALUE, &allowed_hosts, token));
        Self { service, guard }
    }
}

/// `GET /rulebook`, `GET /api/rulebook?repo=`, `GET /api/rulebook/repos`,
/// `POST /api/rulebook/{repo}/rules/{id}/decision`, `POST /api/rulebook/{repo}/rebuild`.
/// `{repo}` is the URL-encoded repository key. No fallback (the dashboard has it).
pub fn router(state: RulebookApiState) -> Router {
    Router::new()
        .route("/rulebook", get(handlers::page))
        .route("/api/rulebook", get(handlers::get_rulebook))
        .route("/api/rulebook/repos", get(handlers::repos))
        .route("/api/rulebook/{repo}/rules/{id}/decision", post(handlers::decide))
        .route("/api/rulebook/{repo}/rebuild", post(handlers::rebuild))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard::protect_mutations))
        .route_layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

#[cfg(test)]
mod tests;
