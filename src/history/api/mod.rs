//! Local web dashboard: thin axum handlers over the history use cases plus one
//! self-contained HTML page.

mod handlers;
mod security;

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::middleware;
use axum::routing::get;
use tokio::net::TcpListener;

use crate::history::application::RunRepository;

/// Native default: loopback only. Containers pass `0.0.0.0:8080` explicitly.
pub const DEFAULT_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
/// Host names (port-insensitive) the dashboard answers to by default.
pub const DEFAULT_ALLOWED_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

#[derive(Clone)]
pub struct DashboardState {
    repository: Arc<dyn RunRepository>,
    allowed_hosts: Arc<[String]>,
}

impl DashboardState {
    /// Uses [`DEFAULT_ALLOWED_HOSTS`].
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self::with_allowed_hosts(repository, DEFAULT_ALLOWED_HOSTS.map(String::from).to_vec())
    }

    /// `allowed_hosts`: host names without port, e.g. `["localhost", "127.0.0.1", "qgate.local"]`.
    /// Matching is case-insensitive and ignores the port. Requests with any
    /// other `Host` are rejected (DNS-rebinding protection).
    pub fn with_allowed_hosts(repository: Arc<dyn RunRepository>, allowed_hosts: Vec<String>) -> Self {
        let allowed_hosts =
            allowed_hosts.iter().map(|h| security::normalize_host(h)).filter(|h| !h.is_empty()).collect();
        Self { repository, allowed_hosts }
    }
}

pub fn router(state: DashboardState) -> Router {
    router_with(state, Router::new())
}

/// The dashboard plus `extra` routes (e.g. the settings page), which get the same Host check and
/// security headers. `extra` must not define a fallback or reuse a dashboard path.
pub fn router_with(state: DashboardState, extra: Router) -> Router {
    router_guarded(state, extra, |app| app)
}

/// [`router_with`], with `guard` wrapped around every route and the fallback, inside the Host check and
/// security headers (the service passes the access context's session check here).
pub fn router_guarded(state: DashboardState, extra: Router, guard: impl FnOnce(Router) -> Router) -> Router {
    let app = Router::new()
        .route("/", get(handlers::index))
        .route("/api/summary", get(handlers::summary))
        .route("/api/runs", get(handlers::list_runs))
        .route("/api/runs/{id}", get(handlers::get_run))
        .route("/api/recurring", get(handlers::recurring))
        .route("/api/tickets/{key}", get(handlers::get_ticket))
        .with_state(state.clone())
        .merge(extra)
        .fallback(handlers::not_found);
    guard(app)
        .layer(middleware::from_fn_with_state(state, security::check_host))
        .layer(middleware::map_response(security::security_headers))
}

/// Binds `addr` (caller decides: [`DEFAULT_ADDR`] natively, `0.0.0.0:<port>` in a container)
/// and serves until `shutdown` resolves.
pub async fn serve(
    addr: SocketAddr,
    state: DashboardState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_on(TcpListener::bind(addr).await?, state, shutdown).await
}

/// Same as [`serve`] on an already bound listener (lets callers bind port 0 and read the address).
pub async fn serve_on(
    listener: TcpListener,
    state: DashboardState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_on_with(listener, state, Router::new(), shutdown).await
}

/// Same as [`serve_on`] with `extra` routes merged in (see [`router_with`]).
pub async fn serve_on_with(
    listener: TcpListener,
    state: DashboardState,
    extra: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_router(listener, router_with(state, extra), shutdown).await
}

/// Serves a router built by [`router_with`] / [`router_guarded`] until `shutdown` resolves.
pub async fn serve_router(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router).with_graceful_shutdown(shutdown).await
}

#[cfg(test)]
mod tests;
