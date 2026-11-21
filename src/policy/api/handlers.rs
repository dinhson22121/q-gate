//! Thin handlers: parse, call one use case, map the result.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{Response, StatusCode};
use axum::response::{Html, IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::PolicyApiState;
use crate::policy::application::PolicyError;
use crate::policy::domain::{ApprovalRequest, RuleApproval};

const POLICY_HTML: &str = include_str!("assets/policy.html");
const SETTINGS_HINT: &str = " Set the Anthropic API key on the Settings page (/settings).";

pub(super) fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Boxed: a `Response` is large and this is the `Err` side of every handler.
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

impl From<PolicyError> for ApiError {
    fn from(err: PolicyError) -> Self {
        match err {
            PolicyError::Invalid(message) => Self::new(StatusCode::UNPROCESSABLE_ENTITY, &message),
            PolicyError::NotFound(message) => Self::new(StatusCode::NOT_FOUND, &message),
            PolicyError::Conflict(message) => Self::new(StatusCode::CONFLICT, &message),
            PolicyError::NeedsConfirmation(message) => Self::new(StatusCode::PRECONDITION_REQUIRED, &message),
            PolicyError::ModelUnavailable(message) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, &format!("{message}.{SETTINGS_HINT}"))
            }
            PolicyError::Model(message) => {
                eprintln!("qgate policy: {message}");
                Self::new(StatusCode::BAD_GATEWAY, "the policy model failed; try again")
            }
            PolicyError::Storage(message) => {
                eprintln!("qgate policy: {message}");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        }
    }
}

type ApiResult = Result<Response<Body>, ApiError>;

fn ok(value: impl Serialize) -> ApiResult {
    Ok(Json(value).into_response())
}

fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, &format!("invalid request body: {e}")))
}

fn parse_id(raw: &str) -> Result<i64, ApiError> {
    raw.parse().map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid proposal id"))
}

/// The page with this instance's API token in its `qgate-token` meta tag (served `Cache-Control: no-store`).
pub(super) async fn page(State(state): State<PolicyApiState>) -> Html<String> {
    Html(state.guard.token().inject(POLICY_HTML))
}

pub(super) async fn repos(State(state): State<PolicyApiState>) -> ApiResult {
    ok(json!({ "repos": state.service.repos().await? }))
}

pub(super) async fn recent(State(state): State<PolicyApiState>) -> ApiResult {
    ok(json!({ "proposals": state.service.recent().await? }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeBody {
    description: String,
    #[serde(default)]
    repo: Option<String>,
}

pub(super) async fn propose(State(state): State<PolicyApiState>, body: Bytes) -> ApiResult {
    let request: ProposeBody = parse_body(&body)?;
    let view = state.service.propose(&request.description, request.repo).await?;
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

pub(super) async fn get_proposal(State(state): State<PolicyApiState>, Path(id): Path<String>) -> ApiResult {
    ok(state.service.get(parse_id(&id)?).await?)
}

#[derive(Deserialize)]
pub(super) struct DryRunQuery {
    limit: Option<u32>,
}

pub(super) async fn dry_run(
    State(state): State<PolicyApiState>,
    Path(id): Path<String>,
    Query(query): Query<DryRunQuery>,
) -> ApiResult {
    ok(state.service.dry_run(parse_id(&id)?, query.limit).await?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleChoice {
    index: usize,
    status: RuleApproval,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveBody {
    #[serde(default)]
    settings: Vec<usize>,
    #[serde(default)]
    rules: Vec<RuleChoice>,
    #[serde(default)]
    confirm_sensitive: bool,
}

pub(super) async fn approve(State(state): State<PolicyApiState>, Path(id): Path<String>, body: Bytes) -> ApiResult {
    let id = parse_id(&id)?;
    let body: ApproveBody = parse_body(&body)?;
    let request = ApprovalRequest {
        settings: body.settings,
        rules: body.rules.into_iter().map(|r| (r.index, r.status)).collect(),
        confirm_sensitive: body.confirm_sensitive,
    };
    ok(state.service.approve(id, request).await?)
}

pub(super) async fn reject(State(state): State<PolicyApiState>, Path(id): Path<String>) -> ApiResult {
    ok(state.service.reject(parse_id(&id)?).await?)
}

pub(super) async fn repreview(State(state): State<PolicyApiState>, Path(id): Path<String>) -> ApiResult {
    let view = state.service.repreview(parse_id(&id)?).await?;
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

#[derive(Deserialize)]
pub(super) struct PrefillQuery {
    run: i64,
    finding: usize,
}

pub(super) async fn prefill(State(state): State<PolicyApiState>, Query(query): Query<PrefillQuery>) -> ApiResult {
    ok(state.service.prefill(query.run, query.finding).await?)
}
