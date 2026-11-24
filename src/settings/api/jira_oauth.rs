use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::ORIGIN;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use super::SettingsApiState;
use super::handlers::{ApiError, ApiResult, blocking, error_response};
use crate::settings::application::SettingsError;
use crate::tickets::application::OauthError;

const CALLBACK_PATH: &str = "/api/jira/oauth/callback";
const CONNECTED_REDIRECT: &str = "/settings?jira=connected";
const FAILED_REDIRECT: &str = "/settings?jira=error";
const CONNECT_BODY_HINT: &str = "body must be {\"site\": \"https://<your-site>.atlassian.net\"}";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectRequest {
    site: String,
}

#[derive(Deserialize)]
pub(super) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

fn redirect_uri(headers: &HeaderMap) -> Option<String> {
    let origin = Url::parse(headers.get(ORIGIN)?.to_str().ok()?).ok()?;
    let base = origin.origin();
    base.is_tuple().then(|| format!("{}{CALLBACK_PATH}", base.ascii_serialization()))
}

fn failure(error: &OauthError) -> Response {
    match error {
        OauthError::Site(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string(), Some("site")),
        OauthError::Discovery(_) | OauthError::Registration(_) | OauthError::Token(_) => {
            eprintln!("qgate jira oauth: {error}");
            error_response(StatusCode::BAD_GATEWAY, "could not reach the Jira authorization server", None)
        }
        OauthError::UnknownState => error_response(StatusCode::BAD_REQUEST, &error.to_string(), None),
        OauthError::Storage(detail) => {
            eprintln!("qgate jira oauth: {detail}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error", None)
        }
    }
}

pub(super) async fn connect(State(state): State<SettingsApiState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let request: ConnectRequest = serde_json::from_slice(&body)
        .map_err(|_| ApiError::new(error_response(StatusCode::UNPROCESSABLE_ENTITY, CONNECT_BODY_HINT, None)))?;
    let redirect_uri = redirect_uri(&headers).ok_or_else(|| {
        ApiError::new(error_response(StatusCode::BAD_REQUEST, "missing or invalid Origin header", None))
    })?;
    let authorize_url =
        state.oauth.connect(&request.site, &redirect_uri).await.map_err(|e| ApiError::new(failure(&e)))?;
    Ok(Json(json!({ "authorize_url": authorize_url })))
}

pub(super) async fn callback(State(state): State<SettingsApiState>, Query(query): Query<CallbackQuery>) -> Response {
    let (Some(code), Some(returned_state)) = (query.code, query.state) else {
        return Redirect::to(FAILED_REDIRECT).into_response();
    };
    match state.oauth.complete(&returned_state, &code, state.deps.credentials()).await {
        Ok(()) => Redirect::to(CONNECTED_REDIRECT).into_response(),
        Err(error) => {
            eprintln!("qgate jira oauth: callback failed: {error}");
            Redirect::to(FAILED_REDIRECT).into_response()
        }
    }
}

pub(super) async fn status(State(state): State<SettingsApiState>) -> ApiResult {
    let oauth = state.oauth.clone();
    blocking(&state, move |deps| {
        let status = oauth.status(deps.credentials()).map_err(|e| SettingsError::Storage(e.to_string()))?;
        Ok(json!({ "connected": status.connected, "site": status.site }))
    })
    .await
}
