use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Serialize;
use serde_json::json;

use super::EvalApiState;
use super::views::{RunDetailView, RunItem, StatusBody};
use crate::eval::application::EvalError;

const DEFAULT_LIMIT: u32 = 30;
const EVAL_HTML: &str = include_str!("assets/eval.html");

pub(super) async fn page() -> Html<&'static str> {
    Html(EVAL_HTML)
}

pub(super) enum ApiError {
    BadRequest(String),
    NotFound,
    Internal,
}

impl From<EvalError> for ApiError {
    fn from(error: EvalError) -> Self {
        match error {
            EvalError::InvalidInput(message) => Self::BadRequest(message),
            other => {
                eprintln!("qgate eval api: {other}");
                Self::Internal
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

async fn blocking<T, F>(work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, EvalError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| {
            eprintln!("qgate eval api: worker failed: {e}");
            ApiError::Internal
        })?
        .map_err(ApiError::from)
}

#[derive(Serialize)]
pub(super) struct RunList {
    runs: Vec<RunItem>,
}

pub(super) async fn list_runs(
    State(state): State<EvalApiState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<RunList>, ApiError> {
    let limit = match params.get("limit").map(|v| v.trim()).filter(|v| !v.is_empty()) {
        None => DEFAULT_LIMIT,
        Some(raw) => raw.parse().map_err(|_| ApiError::BadRequest("invalid limit".into()))?,
    };
    let entries = blocking(move || state.list.execute(limit)).await?;
    Ok(Json(RunList { runs: entries.iter().map(RunItem::from).collect() }))
}

pub(super) async fn get_run(
    State(state): State<EvalApiState>,
    Path(id): Path<i64>,
) -> Result<Json<RunDetailView>, ApiError> {
    let detail = blocking(move || state.get.execute(id)).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(detail.into()))
}

pub(super) async fn status(State(state): State<EvalApiState>) -> Result<Json<StatusBody>, ApiError> {
    let current = state.current.current().await;
    let repo_models = state.repo_models.list().await;
    let view = blocking(move || state.status.execute(current, &repo_models)).await?;
    Ok(Json(view.into()))
}
