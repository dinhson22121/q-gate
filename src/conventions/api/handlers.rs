use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{Response, StatusCode};
use axum::response::{Html, IntoResponse};
use serde::Deserialize;
use serde_json::{Value, json};

use super::RulebookApiState;
use crate::conventions::application::ConventionsError;
use crate::conventions::domain::rulebook::{RuleStatus, Rulebook};

const RULEBOOK_HTML: &str = include_str!("assets/rulebook.html");
const DECISION_HINT: &str = "body must be {\"status\": \"approved\" | \"rejected\" | \"proposed\"}";

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

impl From<ConventionsError> for ApiError {
    fn from(err: ConventionsError) -> Self {
        match err {
            ConventionsError::NotFound(message) => Self::new(StatusCode::NOT_FOUND, &message),
            ConventionsError::Conflict(message) => Self::new(StatusCode::CONFLICT, &message),
            other => {
                eprintln!("qgate conventions: {other}");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        }
    }
}

type ApiResult = Result<Response<Body>, ApiError>;

fn to_json(rulebook: &Rulebook) -> Result<Value, ApiError> {
    serde_json::to_value(rulebook).map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
}

pub(super) async fn page(State(state): State<RulebookApiState>) -> Html<String> {
    Html(state.guard.token().inject(RULEBOOK_HTML))
}

pub(super) async fn repos(State(state): State<RulebookApiState>) -> ApiResult {
    let repos = state.service.repos().await?;
    Ok(Json(json!({ "repos": repos })).into_response())
}

#[derive(Deserialize)]
pub(super) struct RepoQuery {
    repo: Option<String>,
}

fn view(state: &RulebookApiState, repo: &str, rulebook: Value) -> Value {
    let build = state.service.build_state(repo);
    json!({ "repo": repo, "rulebook": rulebook, "building": build.building, "last_error": build.last_error })
}

pub(super) async fn get_rulebook(State(state): State<RulebookApiState>, Query(query): Query<RepoQuery>) -> ApiResult {
    let repo = query.repo.filter(|r| !r.trim().is_empty());
    let repo = repo.ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "query parameter repo is required"))?;
    let rulebook = state.service.load(&repo).await?;
    let Some(rulebook) = rulebook else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no rulebook for this repository"));
    };
    let rulebook = to_json(&rulebook)?;
    Ok(Json(view(&state, &repo, rulebook)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionBody {
    status: String,
}

pub(super) async fn decide(
    State(state): State<RulebookApiState>,
    Path((repo, id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let parsed: DecisionBody =
        serde_json::from_slice(&body).map_err(|_| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, DECISION_HINT))?;
    let status = RuleStatus::parse(&parsed.status)
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, DECISION_HINT))?;
    let rulebook = state.service.decide(&repo, &id, status).await?;
    let rulebook = to_json(&rulebook)?;
    Ok(Json(view(&state, &repo, rulebook)).into_response())
}

pub(super) async fn rebuild(State(state): State<RulebookApiState>, Path(repo): Path<String>) -> ApiResult {
    state.service.start_rebuild(&repo).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "repo": repo, "building": true }))).into_response())
}
