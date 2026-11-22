use std::time::{Duration, Instant};

use futures::future::BoxFuture;

use super::error::RedteamError;
use super::ports::HttpProbe;
use crate::redteam::domain::{
    AttackSurface, Endpoint, EndpointSource, ProbeRequest, ProbeResponse, extend_from_openapi,
};

const COMMON_PATHS: [&str; 8] =
    ["/api", "/api/v1", "/admin", "/actuator/health", "/graphql", "/metrics", "/status", "/debug"];
const OPENAPI_PATHS: [&str; 4] = ["/openapi.json", "/swagger.json", "/v3/api-docs", "/api-docs"];

pub struct ReconOutcome {
    pub surface: AttackSurface,
    pub baseline_elapsed: Duration,
    pub http_requests: u32,
    pub notes: Vec<String>,
}

pub fn recon<'a>(
    probe: &'a dyn HttpProbe,
    base_url: &'a str,
    health_path: &'a str,
    auth_headers: &'a [(String, String)],
    max_requests: u32,
    deadline: Instant,
) -> BoxFuture<'a, ReconOutcome> {
    Box::pin(async move {
        let mut surface = AttackSurface::default();
        let mut notes = Vec::new();
        let mut http_requests = 0u32;
        let mut baseline_elapsed = Duration::ZERO;

        if let Some(response) = probe_path(probe, base_url, health_path, auth_headers, &mut http_requests).await {
            baseline_elapsed = response.elapsed;
            surface.add(Endpoint::new("GET", health_path, EndpointSource::HealthCheck));
        } else {
            notes.push(format!("health check at {health_path} did not respond"));
        }

        for path in COMMON_PATHS {
            if http_requests >= max_requests || Instant::now() >= deadline {
                break;
            }
            if let Some(response) = probe_path(probe, base_url, path, auth_headers, &mut http_requests).await
                && response.status != 404
            {
                surface.add(Endpoint::new("GET", path, EndpointSource::CommonPath));
            }
        }

        for path in OPENAPI_PATHS {
            if http_requests >= max_requests || Instant::now() >= deadline {
                break;
            }
            if let Some(response) = probe_path(probe, base_url, path, auth_headers, &mut http_requests).await
                && response.status == 200
                && let Ok(document) = serde_json::from_str(&response.body)
            {
                extend_from_openapi(&mut surface, &document);
            }
        }

        ReconOutcome { surface, baseline_elapsed, http_requests, notes }
    })
}

async fn probe_path(
    probe: &dyn HttpProbe,
    base_url: &str,
    path: &str,
    auth_headers: &[(String, String)],
    http_requests: &mut u32,
) -> Option<ProbeResponse> {
    let request = ProbeRequest {
        method: "GET".into(),
        url: join_url(base_url, path),
        headers: auth_headers.to_vec(),
        body: None,
    };
    *http_requests += 1;
    probe.send(&request).await.ok()
}

pub fn join_url(base_url: &str, path: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), path.trim_start_matches('/'))
}

pub fn probe_error_note(err: &RedteamError) -> String {
    format!("probe error: {err}")
}

#[cfg(test)]
mod tests {
    use futures::future::BoxFuture;

    use super::*;

    struct FakeProbe {
        responses: std::collections::HashMap<String, ProbeResponse>,
    }

    impl HttpProbe for FakeProbe {
        fn send<'a>(&'a self, request: &'a ProbeRequest) -> BoxFuture<'a, Result<ProbeResponse, RedteamError>> {
            let found = self.responses.get(&request.url).cloned();
            Box::pin(async move { found.ok_or(RedteamError::Probe("no route".into())) })
        }
    }

    fn ok(status: u16, body: &str) -> ProbeResponse {
        ProbeResponse { status, headers: vec![], body: body.into(), elapsed: Duration::from_millis(5) }
    }

    #[tokio::test]
    async fn discovers_common_paths_that_do_not_404_and_parses_openapi() {
        let mut responses = std::collections::HashMap::new();
        responses.insert("http://t/health".into(), ok(200, "ok"));
        responses.insert("http://t/admin".into(), ok(200, "ok"));
        responses.insert("http://t/openapi.json".into(), ok(200, r#"{"paths":{"/widgets":{"get":{}}}}"#));
        let probe = FakeProbe { responses };
        let outcome = recon(&probe, "http://t", "/health", &[], 100, Instant::now() + Duration::from_secs(10)).await;
        let paths: Vec<&str> = outcome.surface.endpoints.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"/health"));
        assert!(paths.contains(&"/admin"));
        assert!(paths.contains(&"/widgets"));
        assert!(outcome.notes.is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_health_check_is_noted() {
        let probe = FakeProbe { responses: std::collections::HashMap::new() };
        let outcome = recon(&probe, "http://t", "/health", &[], 100, Instant::now() + Duration::from_secs(10)).await;
        assert!(outcome.surface.endpoints.is_empty());
        assert_eq!(outcome.notes, vec!["health check at /health did not respond"]);
    }

    #[tokio::test]
    async fn stops_probing_once_the_http_budget_is_spent() {
        let mut responses = std::collections::HashMap::new();
        responses.insert("http://t/health".into(), ok(200, "ok"));
        let probe = FakeProbe { responses };
        let outcome = recon(&probe, "http://t", "/health", &[], 1, Instant::now() + Duration::from_secs(10)).await;
        assert_eq!(outcome.http_requests, 1);
    }

    #[test]
    fn join_url_normalizes_the_single_slash() {
        assert_eq!(join_url("http://t/", "/x"), "http://t/x");
        assert_eq!(join_url("http://t", "x"), "http://t/x");
    }
}
