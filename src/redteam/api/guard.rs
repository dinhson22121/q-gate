use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::Response;
use axum::middleware::Next;

use super::RedteamApiState;
use super::handlers::error_response;

pub const REQUEST_HEADER_VALUE: &str = "redteam";

pub(super) async fn protect_mutations(
    State(state): State<RedteamApiState>,
    request: Request,
    next: Next,
) -> Response<Body> {
    match state.guard.check(request.method(), request.headers()) {
        Ok(()) => next.run(request).await,
        Err(rejection) => error_response(rejection.status, rejection.message),
    }
}
