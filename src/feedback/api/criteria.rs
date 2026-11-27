use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;

use super::FeedbackApiState;
use super::handlers::{ApiError, ApiResult, blocking, parse};
use crate::feedback::application::CriterionVerdict;

const VERIFY_HINT: &str = "body must be {\"repo\": string, \"criterion_id\": string, \"criterion_text\": string, \"verdict\": \"verified_met\" | \"not_applicable\" | \"clarified\", \"note\"?: string}";
const REVOKE_HINT: &str = "body must be {\"repo\": string, \"criterion_id\": string, \"criterion_text\": string}";
const MAX_REPO_CHARS: usize = 512;
const MAX_ID_CHARS: usize = 200;
const MAX_TEXT_CHARS: usize = 4000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyBody {
    repo: String,
    criterion_id: String,
    criterion_text: String,
    verdict: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeBody {
    repo: String,
    criterion_id: String,
    criterion_text: String,
}

#[derive(Deserialize)]
pub(super) struct ListQuery {
    repo: String,
}

fn check_lengths(repo: &str, criterion_id: &str, criterion_text: &str) -> Result<(), ApiError> {
    if repo.chars().count() > MAX_REPO_CHARS {
        return Err(ApiError::bad_request("repo must be at most 512 characters"));
    }
    if criterion_id.chars().count() > MAX_ID_CHARS {
        return Err(ApiError::bad_request("criterion_id must be at most 200 characters"));
    }
    if criterion_text.chars().count() > MAX_TEXT_CHARS {
        return Err(ApiError::bad_request("criterion_text must be at most 4000 characters"));
    }
    Ok(())
}

pub(super) async fn verify(State(state): State<FeedbackApiState>, body: Bytes) -> ApiResult {
    let parsed: VerifyBody = parse(&body, VERIFY_HINT)?;
    let verdict = CriterionVerdict::parse(&parsed.verdict).ok_or_else(|| ApiError::bad_request(VERIFY_HINT))?;
    check_lengths(&parsed.repo, &parsed.criterion_id, &parsed.criterion_text)?;
    let criteria = state.criteria.clone();
    let stored = blocking(move || {
        criteria.verify(
            &parsed.repo,
            &parsed.criterion_id,
            &parsed.criterion_text,
            verdict,
            parsed.note.as_deref().unwrap_or_default(),
        )
    })
    .await?;
    Ok(Json(stored).into_response())
}

pub(super) async fn revoke(State(state): State<FeedbackApiState>, body: Bytes) -> ApiResult {
    let parsed: RevokeBody = parse(&body, REVOKE_HINT)?;
    check_lengths(&parsed.repo, &parsed.criterion_id, &parsed.criterion_text)?;
    let criteria = state.criteria.clone();
    let removed = blocking(move || criteria.revoke(&parsed.repo, &parsed.criterion_id, &parsed.criterion_text)).await?;
    Ok(Json(json!({ "removed": removed })).into_response())
}

pub(super) async fn list(State(state): State<FeedbackApiState>, Query(query): Query<ListQuery>) -> ApiResult {
    check_lengths(&query.repo, "", "")?;
    let criteria = state.criteria.clone();
    let items = blocking(move || criteria.list(&query.repo)).await?;
    Ok(Json(json!({ "items": items })).into_response())
}
