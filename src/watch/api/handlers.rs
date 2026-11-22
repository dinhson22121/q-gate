use std::path::Path;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Query, Request, State};
use axum::http::{Response, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ReposApiState;
use crate::watch::application::manage_repos::{MAX_IMPORTED_REPOS, ManageError, RepoBrowser};
use crate::watch::application::ports::{RepoRegistry, WatchError, WatchedRepo};
use crate::watch::domain::browse::Listing;

const REPOS_HTML: &str = include_str!("assets/repos.html");
const PATH_HINT: &str = "body must be {\"path\": \"<folder>\"}";

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
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

fn manage_error(err: ManageError) -> ApiError {
    match err {
        ManageError::OutsideBase => ApiError::new(StatusCode::FORBIDDEN, &err.to_string()),
        ManageError::NotFound(_) => ApiError::new(StatusCode::NOT_FOUND, &err.to_string()),
        ManageError::TooManyRepos(_) => ApiError::new(StatusCode::CONFLICT, &err.to_string()),
        ManageError::NotADirectory(_) | ManageError::NotGitRoot(_) | ManageError::Watch(WatchError::InvalidRepo(_)) => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, &err.to_string())
        }
        other => {
            eprintln!("qgate repos: {other}");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

type ApiResult = Result<Response<Body>, ApiError>;

pub(super) async fn protect_mutations<R: RepoRegistry, B: RepoBrowser>(
    State(state): State<ReposApiState<R, B>>,
    request: Request,
    next: Next,
) -> Response<Body> {
    match state.guard.check(request.method(), request.headers()) {
        Ok(()) => next.run(request).await,
        Err(rejection) => error_response(rejection.status, rejection.message),
    }
}

pub(super) async fn page<R: RepoRegistry, B: RepoBrowser>(State(state): State<ReposApiState<R, B>>) -> Html<String> {
    Html(state.guard.token().inject(REPOS_HTML))
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn listing_json(listing: &Listing) -> Value {
    json!({
        "base": text(&listing.base),
        "path": text(&listing.path),
        "parent": listing.parent.as_deref().map(text),
        "is_git_root": listing.is_git_root,
        "truncated": listing.truncated,
        "crumbs": listing.crumbs.iter().map(|c| json!({ "name": c.name, "path": text(&c.path) })).collect::<Vec<_>>(),
        "entries": listing.entries.iter()
            .map(|e| json!({ "name": e.name, "path": text(&e.path), "is_git_root": e.is_git_root }))
            .collect::<Vec<_>>(),
    })
}

fn repo_json(repo: &WatchedRepo) -> Value {
    json!({ "name": repo.name, "path": text(&repo.path), "exists": repo.path.is_dir() })
}

#[derive(Deserialize)]
pub(super) struct BrowseQuery {
    path: Option<String>,
}

pub(super) async fn browse<R: RepoRegistry, B: RepoBrowser>(
    State(state): State<ReposApiState<R, B>>,
    Query(query): Query<BrowseQuery>,
) -> ApiResult {
    let listing = state.service.browse(query.path.as_deref().unwrap_or("")).await.map_err(manage_error)?;
    Ok(Json(listing_json(&listing)).into_response())
}

pub(super) async fn list<R: RepoRegistry, B: RepoBrowser>(State(state): State<ReposApiState<R, B>>) -> ApiResult {
    let repos = state.service.list().await.map_err(manage_error)?;
    let repos: Vec<Value> = repos.iter().map(repo_json).collect();
    Ok(Json(json!({ "repos": repos, "max_repos": MAX_IMPORTED_REPOS })).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathBody {
    path: String,
}

fn parse_path(body: &Bytes) -> Result<String, ApiError> {
    serde_json::from_slice::<PathBody>(body)
        .map(|b| b.path)
        .map_err(|_| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, PATH_HINT))
}

pub(super) async fn import<R: RepoRegistry, B: RepoBrowser>(
    State(state): State<ReposApiState<R, B>>,
    body: Bytes,
) -> ApiResult {
    let path = parse_path(&body)?;
    let repo = state.service.import(&path).await.map_err(manage_error)?;
    Ok((StatusCode::CREATED, Json(json!({ "repo": repo_json(&repo) }))).into_response())
}

pub(super) async fn remove<R: RepoRegistry, B: RepoBrowser>(
    State(state): State<ReposApiState<R, B>>,
    body: Bytes,
) -> ApiResult {
    let path = parse_path(&body)?;
    if state.service.remove(Path::new(&path)).await.map_err(manage_error)? {
        Ok(Json(json!({ "removed": true })).into_response())
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "that repository is not imported"))
    }
}
