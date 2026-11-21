use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::SettingsApiState;
use crate::settings::application::server_settings::{
    ClearSecret, GetSettings, RollbackSettings, SetSecret, SettingsHistory, UpdateSettings, WebhookChange,
};
use crate::settings::application::{CredentialName, ServerConfig, SettingsDeps, SettingsError};

const SETTINGS_HTML: &str = include_str!("assets/settings.html");
const HISTORY_LIMIT: u32 = 20;
const SECRET_BODY_HINT: &str = "body must be {\"value\": \"<secret>\"}";

pub(super) fn error_response(status: StatusCode, message: &str, field: Option<&str>) -> Response {
    (status, Json(json!({ "error": message, "field": field }))).into_response()
}

pub(super) struct ApiError(Box<Response>);

impl ApiError {
    fn new(response: Response) -> Self {
        Self(Box::new(response))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        *self.0
    }
}

impl From<SettingsError> for ApiError {
    fn from(err: SettingsError) -> Self {
        let message = err.to_string();
        let response = match &err {
            SettingsError::Invalid(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e.message, Some(&e.field)),
            SettingsError::Locked { field, .. } => error_response(StatusCode::CONFLICT, &message, Some(field)),
            SettingsError::AdminOnly { field, .. } => error_response(StatusCode::BAD_REQUEST, &message, Some(field)),
            SettingsError::SecretFromEnv { name, .. } => {
                error_response(StatusCode::CONFLICT, &message, Some(&format!("secrets.{name}")))
            }
            SettingsError::InvalidSecret(_) => {
                error_response(StatusCode::UNPROCESSABLE_ENTITY, &message, Some("value"))
            }
            SettingsError::VersionNotFound(_) => error_response(StatusCode::NOT_FOUND, &message, None),
            SettingsError::Conflict(_) => error_response(StatusCode::CONFLICT, &message, None),
            SettingsError::Storage(detail) => {
                eprintln!("qgate settings: {detail}");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error", None)
            }
        };
        Self(Box::new(response))
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

async fn blocking<F>(state: &SettingsApiState, work: F) -> ApiResult
where
    F: FnOnce(&SettingsDeps) -> Result<Value, SettingsError> + Send + 'static,
{
    let deps = state.deps.clone();
    let result = tokio::task::spawn_blocking(move || work(&deps)).await.map_err(|err| {
        eprintln!("qgate settings: worker failed: {err}");
        ApiError::new(error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error", None))
    })?;
    Ok(Json(result?))
}

fn to_value(value: impl serde::Serialize) -> Result<Value, SettingsError> {
    serde_json::to_value(value).map_err(|e| SettingsError::Storage(format!("cannot serialize response: {e}")))
}

fn unprocessable(message: String) -> ApiError {
    ApiError::new(error_response(StatusCode::UNPROCESSABLE_ENTITY, &message, None))
}

#[derive(Deserialize, Default)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum WebhookDto {
    #[default]
    Keep,
    Clear,
    Set {
        url: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRequest {
    config: ServerConfig,
    #[serde(default)]
    webhook: WebhookDto,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretRequest {
    value: String,
}

fn secret_name(raw: &str) -> Result<CredentialName, ApiError> {
    CredentialName::from_key(raw)
        .ok_or_else(|| ApiError::new(error_response(StatusCode::NOT_FOUND, "unknown secret", None)))
}

fn applied_with_view(deps: &SettingsDeps, applied: impl serde::Serialize) -> Result<Value, SettingsError> {
    Ok(json!({ "applied": to_value(applied)?, "settings": to_value(GetSettings::new(deps).execute()?)? }))
}

pub(super) async fn page(State(state): State<SettingsApiState>) -> Html<String> {
    Html(state.guard.token().inject(SETTINGS_HTML))
}

pub(super) async fn get_settings(State(state): State<SettingsApiState>) -> ApiResult {
    blocking(&state, |deps| to_value(GetSettings::new(deps).execute()?)).await
}

pub(super) async fn put_settings(State(state): State<SettingsApiState>, body: Bytes) -> ApiResult {
    let request: UpdateRequest =
        serde_json::from_slice(&body).map_err(|e| unprocessable(format!("invalid request body: {e}")))?;
    let webhook = match request.webhook {
        WebhookDto::Keep => WebhookChange::Keep,
        WebhookDto::Clear => WebhookChange::Clear,
        WebhookDto::Set { url } => WebhookChange::Set(url),
    };
    blocking(&state, move |deps| {
        let applied = UpdateSettings::new(deps).execute(request.config, webhook)?;
        applied_with_view(deps, applied)
    })
    .await
}

pub(super) async fn history(State(state): State<SettingsApiState>) -> ApiResult {
    blocking(&state, |deps| to_value(SettingsHistory::new(deps).execute(HISTORY_LIMIT)?)).await
}

pub(super) async fn rollback(State(state): State<SettingsApiState>, Path(version): Path<String>) -> ApiResult {
    let version: i64 = version
        .parse()
        .map_err(|_| ApiError::new(error_response(StatusCode::BAD_REQUEST, "invalid settings version", None)))?;
    blocking(&state, move |deps| {
        let applied = RollbackSettings::new(deps).execute(version)?;
        applied_with_view(deps, applied)
    })
    .await
}

pub(super) async fn put_secret(
    State(state): State<SettingsApiState>,
    Path(name): Path<String>,
    body: Bytes,
) -> ApiResult {
    let name = secret_name(&name)?;
    let request: SecretRequest = serde_json::from_slice(&body).map_err(|_| unprocessable(SECRET_BODY_HINT.into()))?;
    blocking(&state, move |deps| {
        Ok(json!({ "secret": to_value(SetSecret::new(deps).execute(name, &request.value)?)? }))
    })
    .await
}

pub(super) async fn delete_secret(State(state): State<SettingsApiState>, Path(name): Path<String>) -> ApiResult {
    let name = secret_name(&name)?;
    blocking(&state, move |deps| Ok(json!({ "secret": to_value(ClearSecret::new(deps).execute(name)?)? }))).await
}
