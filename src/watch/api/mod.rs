mod handlers;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};

use crate::watch::application::manage_repos::{ManageRepos, RepoBrowser};
use crate::watch::application::ports::RepoRegistry;
use crate::watch::infrastructure::fs_browser::FsRepoBrowser;
use crate::web::{ApiToken, MutationGuard};

pub const REQUEST_HEADER_VALUE: &str = "repos";

pub const MAX_BODY_BYTES: usize = 8 * 1024;

pub struct ReposApiState<R, B = FsRepoBrowser> {
    service: Arc<ManageRepos<R, B>>,
    guard: Arc<MutationGuard>,
}

impl<R, B> Clone for ReposApiState<R, B> {
    fn clone(&self) -> Self {
        Self { service: self.service.clone(), guard: self.guard.clone() }
    }
}

impl<R: RepoRegistry> ReposApiState<R> {
    pub fn new(registry: R, base: PathBuf, allowed_hosts: Vec<String>, token: ApiToken) -> Self {
        Self::with_browser(registry, FsRepoBrowser::new(base), allowed_hosts, token)
    }
}

impl<R: RepoRegistry, B: RepoBrowser> ReposApiState<R, B> {
    pub fn with_browser(registry: R, browser: B, allowed_hosts: Vec<String>, token: ApiToken) -> Self {
        let guard = Arc::new(MutationGuard::new(REQUEST_HEADER_VALUE, &allowed_hosts, token));
        Self { service: Arc::new(ManageRepos::new(registry, browser)), guard }
    }
}

pub fn router<R, B>(state: ReposApiState<R, B>) -> Router
where
    R: RepoRegistry,
    B: RepoBrowser,
{
    Router::new()
        .route("/repos", get(handlers::page::<R, B>))
        .route("/api/repos", get(handlers::list::<R, B>).delete(handlers::remove::<R, B>))
        .route("/api/repos/browse", get(handlers::browse::<R, B>))
        .route("/api/repos/import", post(handlers::import::<R, B>))
        .route_layer(middleware::from_fn_with_state(state.clone(), handlers::protect_mutations::<R, B>))
        .route_layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

#[cfg(test)]
mod tests;
