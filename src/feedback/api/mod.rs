mod criteria;
mod guard;
mod handlers;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};

use crate::feedback::application::{CriterionVerifications, FeedbackService};
use crate::history::application::RunRepository;
use crate::web::{ApiToken, MutationGuard};

pub use guard::{REQUEST_HEADER, REQUEST_HEADER_VALUE};

pub const MAX_BODY_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct FeedbackApiState {
    service: Arc<FeedbackService>,
    criteria: Arc<CriterionVerifications>,
    runs: Arc<dyn RunRepository>,
    guard: Arc<MutationGuard>,
}

impl FeedbackApiState {
    pub fn new(
        service: Arc<FeedbackService>,
        criteria: Arc<CriterionVerifications>,
        runs: Arc<dyn RunRepository>,
        allowed_hosts: Vec<String>,
        token: ApiToken,
    ) -> Self {
        let guard = Arc::new(MutationGuard::new(guard::REQUEST_HEADER_VALUE, &allowed_hosts, token));
        Self { service, criteria, runs, guard }
    }
}

pub fn router(state: FeedbackApiState) -> Router {
    Router::new()
        .route("/api/feedback/runs/{id}", get(handlers::run_feedback))
        .route("/api/feedback", post(handlers::record))
        .route("/api/feedback/undo", post(handlers::undo))
        .route("/api/feedback/reset", post(handlers::reset))
        .route("/api/feedback/criteria", get(criteria::list).post(criteria::verify).delete(criteria::revoke))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard::protect_mutations))
        .route_layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

#[cfg(test)]
mod tests;
