mod guard;
mod handlers;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::get;

use crate::redteam::application::RedteamService;
use crate::web::{ApiToken, MutationGuard};

pub use crate::web::REQUEST_HEADER;
pub use guard::REQUEST_HEADER_VALUE;

pub const MAX_BODY_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct RedteamApiState {
    service: Arc<RedteamService>,
    guard: Arc<MutationGuard>,
}

impl RedteamApiState {
    pub fn new(service: Arc<RedteamService>, allowed_hosts: Vec<String>, token: ApiToken) -> Self {
        let guard = Arc::new(MutationGuard::new(guard::REQUEST_HEADER_VALUE, &allowed_hosts, token));
        Self { service, guard }
    }
}

pub fn redteam_router(state: RedteamApiState) -> Router {
    Router::new()
        .route("/redteam", get(handlers::page))
        .route("/api/redteam/scan", axum::routing::post(handlers::start_scan))
        .route("/api/redteam/scans", get(handlers::list_scans))
        .route("/api/redteam/scans/{id}", get(handlers::get_scan))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard::protect_mutations))
        .route_layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

#[cfg(test)]
mod tests;
