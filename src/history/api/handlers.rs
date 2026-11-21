use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use super::DashboardState;
use crate::history::application::{
    DashboardSummary, GetRun, HistoryError, ListRuns, RecurringIssues, RunStatus, TicketHistory,
};

const DASHBOARD_HTML: &str = include_str!("assets/dashboard.html");

pub(super) enum ApiError {
    BadRequest(String),
    NotFound,
    Internal,
}

pub(super) fn error_body(message: &str) -> Json<Value> {
    Json(json!({ "error": message }))
}

impl From<HistoryError> for ApiError {
    fn from(err: HistoryError) -> Self {
        match err {
            HistoryError::InvalidInput(message) => Self::BadRequest(message),
            HistoryError::Storage(detail) => {
                eprintln!("qgate dashboard: {detail}");
                Self::Internal
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, error_body(&message)).into_response(),
            Self::NotFound => (StatusCode::NOT_FOUND, error_body("not found")).into_response(),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, error_body("internal error")).into_response(),
        }
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

/// Runs a synchronous use case off the async runtime and serializes its result.
async fn blocking<T, F>(work: F) -> ApiResult
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce() -> Result<T, HistoryError> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(work).await.map_err(|err| {
        eprintln!("qgate dashboard: worker failed: {err}");
        ApiError::Internal
    })??;
    serde_json::to_value(result).map(Json).map_err(|err| {
        eprintln!("qgate dashboard: serialization failed: {err}");
        ApiError::Internal
    })
}

fn parse_param<T: std::str::FromStr>(params: &HashMap<String, String>, name: &str) -> Result<Option<T>, ApiError> {
    match params.get(name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(raw) => raw.parse().map(Some).map_err(|_| ApiError::BadRequest(format!("invalid {name}"))),
    }
}

pub(super) async fn index() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

pub(super) async fn not_found() -> ApiError {
    ApiError::NotFound
}

pub(super) async fn summary(State(state): State<DashboardState>) -> ApiResult {
    blocking(move || DashboardSummary::new(state.repository).execute(Utc::now())).await
}

pub(super) async fn recurring(State(state): State<DashboardState>) -> ApiResult {
    blocking(move || RecurringIssues::new(state.repository).execute(Utc::now())).await
}

pub(super) async fn list_runs(
    State(state): State<DashboardState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let status = parse_param::<RunStatus>(&params, "status")?;
    let limit = parse_param::<u32>(&params, "limit")?;
    let offset = parse_param::<u32>(&params, "offset")?.unwrap_or(0);
    let repo = params.get("repo").cloned();
    blocking(move || ListRuns::new(state.repository).execute(repo, status, limit, offset)).await
}

pub(super) async fn get_run(State(state): State<DashboardState>, Path(id): Path<String>) -> ApiResult {
    let id: i64 = id.parse().map_err(|_| ApiError::BadRequest("invalid run id".into()))?;
    let run = blocking(move || GetRun::new(state.repository).execute(id)).await?;
    if run.0.is_null() { Err(ApiError::NotFound) } else { Ok(run) }
}

/// All runs concerning a Jira ticket plus per-criterion progress; 400 on a malformed key.
pub(super) async fn get_ticket(State(state): State<DashboardState>, Path(key): Path<String>) -> ApiResult {
    blocking(move || TicketHistory::new(state.repository).execute(&key)).await
}
