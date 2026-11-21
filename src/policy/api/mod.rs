mod guard;
mod handlers;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};

use crate::policy::application::PolicyService;
use crate::web::{ApiToken, MutationGuard};

pub use guard::{REQUEST_HEADER, REQUEST_HEADER_VALUE};

pub const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct PolicyApiState {
    service: Arc<PolicyService>,
    guard: Arc<MutationGuard>,
}

impl PolicyApiState {
    pub fn new(service: Arc<PolicyService>, allowed_hosts: Vec<String>, token: ApiToken) -> Self {
        let guard = Arc::new(MutationGuard::new(guard::REQUEST_HEADER_VALUE, &allowed_hosts, token));
        Self { service, guard }
    }
}

pub fn router(state: PolicyApiState) -> Router {
    Router::new()
        .route("/policy", get(handlers::page))
        .route("/api/policy/repos", get(handlers::repos))
        .route("/api/policy/proposals", get(handlers::recent).post(handlers::propose))
        .route("/api/policy/proposals/{id}", get(handlers::get_proposal))
        .route("/api/policy/proposals/{id}/dry-run", get(handlers::dry_run))
        .route("/api/policy/proposals/{id}/approve", post(handlers::approve))
        .route("/api/policy/proposals/{id}/reject", post(handlers::reject))
        .route("/api/policy/proposals/{id}/repreview", post(handlers::repreview))
        .route("/api/policy/prefill", get(handlers::prefill))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard::protect_mutations))
        .route_layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

#[cfg(test)]
mod tests;
