mod guard;
mod handlers;
mod jira_oauth;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post, put};

use crate::settings::application::SettingsDeps;
use crate::tickets::application::{JiraOauthGateway, JiraOauthService};
use crate::web::{ApiToken, MutationGuard};

pub use guard::{REQUEST_HEADER, REQUEST_HEADER_VALUE};

pub const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct SettingsApiState {
    deps: Arc<SettingsDeps>,
    guard: Arc<MutationGuard>,
    oauth: Arc<JiraOauthService<dyn JiraOauthGateway>>,
}

impl SettingsApiState {
    pub fn new(
        deps: Arc<SettingsDeps>,
        allowed_hosts: Vec<String>,
        token: ApiToken,
        oauth_gateway: Arc<dyn JiraOauthGateway>,
    ) -> Self {
        let guard = Arc::new(MutationGuard::new(guard::REQUEST_HEADER_VALUE, &allowed_hosts, token));
        Self { deps, guard, oauth: Arc::new(JiraOauthService::new(oauth_gateway)) }
    }
}

pub fn router(state: SettingsApiState) -> Router {
    Router::new()
        .route("/settings", get(handlers::page))
        .route("/api/settings", get(handlers::get_settings).put(handlers::put_settings))
        .route("/api/settings/history", get(handlers::history))
        .route("/api/settings/rollback/{version}", post(handlers::rollback))
        .route("/api/secrets/{name}", put(handlers::put_secret).delete(handlers::delete_secret))
        .route("/api/jira/oauth/connect", post(jira_oauth::connect))
        .route("/api/jira/oauth/callback", get(jira_oauth::callback))
        .route("/api/jira/oauth/status", get(jira_oauth::status))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard::protect_mutations))
        .route_layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

#[cfg(test)]
mod tests;
