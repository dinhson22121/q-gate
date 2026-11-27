use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{Response, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};

use super::FeedbackApiState;
use crate::feedback::application::{
    DISMISSAL_TTL_DAYS, FeedbackError, FindingFacts, Judgment, Scope, Verdict, fingerprint, kind_of,
};
use crate::history::application::{FindingView, GetRun, HistoryError, StoredRun};

const RECORD_HINT: &str = "body must be {\"run_id\": number, \"index\": number, \"verdict\": \"accepted\" | \"dismissed\", \"scope\"?: \"repo\" | \"global\"}";
const TARGET_HINT: &str = "body must be {\"run_id\": number, \"index\": number}";
const RESET_HINT: &str = "body must be {\"run_id\": number, \"kind\": string}";
const MAX_KIND_CHARS: usize = 200;

pub(super) fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    (status, Json(json!({ "error": message }))).into_response()
}

pub(super) struct ApiError(Box<Response<Body>>);

impl ApiError {
    fn new(status: StatusCode, message: &str) -> Self {
        Self(Box::new(error_response(status, message)))
    }

    pub(super) fn bad_request(message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response<Body> {
        *self.0
    }
}

impl From<FeedbackError> for ApiError {
    fn from(err: FeedbackError) -> Self {
        match err {
            FeedbackError::InvalidInput(message) => Self::bad_request(&message),
            FeedbackError::Storage(detail) => {
                eprintln!("qgate feedback: {detail}");
                Self::internal()
            }
        }
    }
}

impl From<HistoryError> for ApiError {
    fn from(err: HistoryError) -> Self {
        match err {
            HistoryError::InvalidInput(message) => Self::bad_request(&message),
            HistoryError::Storage(detail) => {
                eprintln!("qgate feedback: {detail}");
                Self::internal()
            }
        }
    }
}

pub(super) type ApiResult = Result<Response<Body>, ApiError>;

pub(super) async fn blocking<T, E, F>(work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    E: Send + 'static,
    ApiError: From<E>,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(work).await.map_err(|err| {
        eprintln!("qgate feedback: worker failed: {err}");
        ApiError::internal()
    })?;
    Ok(result?)
}

pub(super) fn facts_of(view: &FindingView) -> FindingFacts {
    let source = match view.source.as_ref() {
        Some(source) if source.kind == "static_tool" => format!("tool:{}", source.name.as_deref().unwrap_or_default()),
        _ => "llm".to_string(),
    };
    FindingFacts {
        file: view.file.clone(),
        category: view.category.as_str().to_string(),
        source,
        evidence: view.evidence.clone(),
    }
}

async fn load_run(state: &FeedbackApiState, id: i64) -> Result<StoredRun, ApiError> {
    let runs = state.runs.clone();
    let run = blocking(move || GetRun::new(runs).execute(id)).await?;
    run.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "run not found"))
}

fn findings_of(run: &StoredRun) -> &[FindingView] {
    run.report.as_ref().map(|report| report.findings.as_slice()).unwrap_or_default()
}

fn facts_at(run: &StoredRun, index: usize) -> Result<FindingFacts, ApiError> {
    findings_of(run)
        .get(index)
        .map(facts_of)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "finding index out of range"))
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(body: &Bytes, hint: &str) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|_| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, hint))
}

pub(super) async fn run_feedback(State(state): State<FeedbackApiState>, Path(id): Path<String>) -> ApiResult {
    let id: i64 = id.parse().map_err(|_| ApiError::bad_request("invalid run id"))?;
    let run = load_run(&state, id).await?;
    let facts: Vec<FindingFacts> = findings_of(&run).iter().map(facts_of).collect();
    let fingerprints: Vec<String> = facts.iter().map(fingerprint).collect();
    let repo = run.repo_path.clone();
    let service = state.service.clone();
    let lookup = fingerprints.clone();
    let (judgments, scores, now) = {
        let repo = repo.clone();
        blocking(move || {
            Ok::<_, FeedbackError>((service.judgments(&repo, &lookup)?, service.scores(&repo)?, service.now()))
        })
        .await?
    };
    let items: Vec<Value> = facts
        .iter()
        .zip(&fingerprints)
        .enumerate()
        .map(|(index, (fact, fingerprint))| {
            let judgment = judgments.iter().find(|j| &j.fingerprint == fingerprint);
            json!({
                "index": index,
                "fingerprint": fingerprint,
                "kind": kind_of(fact),
                "verdict": judgment.and_then(|j| j.effective_verdict(now)).map(Verdict::as_str),
                "scope": judgment.map(|j| j.scope.as_str()),
                "expires_at": judgment.and_then(Judgment::expires_at).map(|at| at.to_rfc3339()),
                "expired": judgment.is_some_and(|j| j.is_expired(now)),
                "inherited": judgment.is_some_and(|j| j.repo != repo),
            })
        })
        .collect();
    Ok(Json(json!({ "repo": repo, "dismissal_ttl_days": DISMISSAL_TTL_DAYS, "items": items, "scores": scores }))
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordBody {
    run_id: i64,
    index: usize,
    verdict: String,
    #[serde(default)]
    scope: Option<String>,
}

pub(super) async fn record(State(state): State<FeedbackApiState>, body: Bytes) -> ApiResult {
    let parsed: RecordBody = parse(&body, RECORD_HINT)?;
    let verdict = Verdict::parse(&parsed.verdict).ok_or_else(|| ApiError::bad_request(RECORD_HINT))?;
    let scope = match parsed.scope.as_deref() {
        None => Scope::default(),
        Some(value) => Scope::parse(value).ok_or_else(|| ApiError::bad_request(RECORD_HINT))?,
    };
    let run = load_run(&state, parsed.run_id).await?;
    let facts = facts_at(&run, parsed.index)?;
    let service = state.service.clone();
    let fingerprint = blocking(move || service.record(&run.repo_path, &facts, verdict, scope)).await?;
    Ok(Json(json!({ "fingerprint": fingerprint, "verdict": verdict.as_str(), "scope": scope.as_str() }))
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetBody {
    run_id: i64,
    index: usize,
}

pub(super) async fn undo(State(state): State<FeedbackApiState>, body: Bytes) -> ApiResult {
    let parsed: TargetBody = parse(&body, TARGET_HINT)?;
    let run = load_run(&state, parsed.run_id).await?;
    let fingerprint = fingerprint(&facts_at(&run, parsed.index)?);
    let service = state.service.clone();
    let removed = blocking(move || service.remove(&run.repo_path, &fingerprint)).await?;
    Ok(Json(json!({ "removed": removed })).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetBody {
    run_id: i64,
    kind: String,
}

pub(super) async fn reset(State(state): State<FeedbackApiState>, body: Bytes) -> ApiResult {
    let parsed: ResetBody = parse(&body, RESET_HINT)?;
    let kind = parsed.kind.trim().to_string();
    if kind.is_empty() || kind.chars().count() > MAX_KIND_CHARS {
        return Err(ApiError::bad_request("kind must be 1-200 characters"));
    }
    let run = load_run(&state, parsed.run_id).await?;
    let service = state.service.clone();
    blocking(move || service.reset(&run.repo_path, &kind)).await?;
    Ok(Json(json!({ "reset": true })).into_response())
}
