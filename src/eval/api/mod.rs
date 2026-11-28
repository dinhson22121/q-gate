mod handlers;
mod views;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use crate::eval::application::{CurrentProvenance, EvalStatus, EvalStore, GetEvalRun, ListEvalRuns, RepoModels};

#[derive(Clone)]
pub struct EvalApiState {
    list: Arc<ListEvalRuns>,
    get: Arc<GetEvalRun>,
    status: Arc<EvalStatus>,
    current: Arc<dyn CurrentProvenance>,
    repo_models: Arc<dyn RepoModels>,
}

impl EvalApiState {
    pub fn new(
        store: Arc<dyn EvalStore>,
        current: Arc<dyn CurrentProvenance>,
        repo_models: Arc<dyn RepoModels>,
    ) -> Self {
        Self {
            list: Arc::new(ListEvalRuns::new(store.clone())),
            get: Arc::new(GetEvalRun::new(store.clone())),
            status: Arc::new(EvalStatus::new(store)),
            current,
            repo_models,
        }
    }
}

pub fn router(state: EvalApiState) -> Router {
    Router::new()
        .route("/eval", get(handlers::page))
        .route("/api/eval/runs", get(handlers::list_runs))
        .route("/api/eval/runs/{id}", get(handlers::get_run))
        .route("/api/eval/status", get(handlers::status))
        .with_state(state)
}

#[cfg(test)]
mod tests;
