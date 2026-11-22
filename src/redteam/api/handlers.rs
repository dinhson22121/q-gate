use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{Response, StatusCode};
use axum::response::{Html, IntoResponse};
use serde::Deserialize;
use serde_json::json;

use super::RedteamApiState;
use crate::redteam::application::RedteamError;
use crate::redteam::domain::ScanTarget;

const REDTEAM_HTML: &str = include_str!("assets/redteam.html");
const DEFAULT_LIST_LIMIT: u32 = 50;

pub(super) fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    (status, Json(json!({ "error": message }))).into_response()
}

pub(super) struct ApiError(Box<Response<Body>>);

impl ApiError {
    fn new(status: StatusCode, message: &str) -> Self {
        Self(Box::new(error_response(status, message)))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response<Body> {
        *self.0
    }
}

impl From<RedteamError> for ApiError {
    fn from(err: RedteamError) -> Self {
        match err {
            RedteamError::HostNotAllowed(_) | RedteamError::InvalidTarget => {
                Self::new(StatusCode::BAD_REQUEST, &err.to_string())
            }
            RedteamError::NotFound => Self::new(StatusCode::NOT_FOUND, &err.to_string()),
            RedteamError::RateLimited => Self::new(StatusCode::TOO_MANY_REQUESTS, &err.to_string()),
            RedteamError::Probe(_) | RedteamError::Model(_) => {
                eprintln!("qgate redteam: {err}");
                Self::new(StatusCode::BAD_GATEWAY, "the scan failed; try again")
            }
            RedteamError::Storage(message) => {
                eprintln!("qgate redteam: {message}");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        }
    }
}

type ApiResult = Result<Response<Body>, ApiError>;

pub(super) async fn page(State(state): State<RedteamApiState>) -> Html<String> {
    Html(state.guard.token().inject(REDTEAM_HTML))
}

fn parse_target(body: &Bytes) -> Result<ScanTarget, ApiError> {
    serde_json::from_slice(body)
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, &format!("invalid scan target: {e}")))
}

pub(super) async fn start_scan(State(state): State<RedteamApiState>, body: Bytes) -> ApiResult {
    let target = parse_target(&body)?;
    let report = state.service.scan(target).await?;
    Ok((StatusCode::CREATED, Json(report)).into_response())
}

#[derive(Deserialize)]
pub(super) struct ListQuery {
    limit: Option<u32>,
    offset: Option<u32>,
}

pub(super) async fn list_scans(State(state): State<RedteamApiState>, Query(query): Query<ListQuery>) -> ApiResult {
    let reports = state.service.list(query.limit.unwrap_or(DEFAULT_LIST_LIMIT), query.offset.unwrap_or(0))?;
    Ok(Json(json!({ "scans": reports })).into_response())
}

fn parse_id(raw: &str) -> Result<i64, ApiError> {
    raw.parse().map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid scan id"))
}

pub(super) async fn get_scan(State(state): State<RedteamApiState>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(state.service.get(parse_id(&id)?)?).into_response())
}
