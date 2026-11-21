//! Settings page and JSON API. Mounted inside the dashboard (`history::api::router_with`) so its Host
//! check and security headers apply; mutating routes add CSRF and API-token checks ([`guard`]).

mod guard;
mod handlers;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post, put};

use crate::settings::application::SettingsDeps;
use crate::web::{ApiToken, MutationGuard};

pub use guard::{REQUEST_HEADER, REQUEST_HEADER_VALUE};

/// Request bodies above this are rejected with 413.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct SettingsApiState {
    deps: Arc<SettingsDeps>,
    guard: Arc<MutationGuard>,
}

impl SettingsApiState {
    /// `allowed_hosts`: the dashboard's `dashboard.allowed_hosts`; an `Origin` header must name one of them.
    /// `token`: the per-instance API token, injected into the page and required on mutating routes.
    pub fn new(deps: Arc<SettingsDeps>, allowed_hosts: Vec<String>, token: ApiToken) -> Self {
        let guard = Arc::new(MutationGuard::new(guard::REQUEST_HEADER_VALUE, &allowed_hosts, token));
        Self { deps, guard }
    }
}

/// `GET /settings`, `GET|PUT /api/settings`, `GET /api/settings/history`,
/// `POST /api/settings/rollback/{version}`, `PUT|DELETE /api/secrets/{name}`. No fallback (the dashboard has it).
pub fn router(state: SettingsApiState) -> Router {
    Router::new()
        .route("/settings", get(handlers::page))
        .route("/api/settings", get(handlers::get_settings).put(handlers::put_settings))
        .route("/api/settings/history", get(handlers::history))
        .route("/api/settings/rollback/{version}", post(handlers::rollback))
        .route("/api/secrets/{name}", put(handlers::put_secret).delete(handlers::delete_secret))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard::protect_mutations))
        .route_layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

#[cfg(test)]
mod tests;
