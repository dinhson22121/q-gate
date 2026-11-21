use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::Response;
use axum::middleware::Next;

use super::RulebookApiState;
use super::handlers::error_response;

pub use crate::web::REQUEST_HEADER;
pub const REQUEST_HEADER_VALUE: &str = "rulebook";

pub(super) async fn protect_mutations(
    State(state): State<RulebookApiState>,
    request: Request,
    next: Next,
) -> Response<Body> {
    match state.guard.check(request.method(), request.headers()) {
        Ok(()) => next.run(request).await,
        Err(rejection) => error_response(rejection.status, rejection.message),
    }
}
