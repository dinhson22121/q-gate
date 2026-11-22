use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use reqwest::{Client, Method, Url, redirect};

use crate::redteam::application::{HttpProbe, RedteamError, TargetPolicy};
use crate::redteam::domain::{ProbeRequest, ProbeResponse};
use crate::web::normalize_host;

const MAX_BODY_CHARS: usize = 1024 * 1024;

pub struct ReqwestProbe {
    client: Client,
    policy: std::sync::Arc<dyn TargetPolicy>,
}

impl ReqwestProbe {
    pub fn new(policy: std::sync::Arc<dyn TargetPolicy>, timeout: Duration) -> Result<Self, RedteamError> {
        let client = Client::builder()
            .timeout(timeout)
            .redirect(redirect::Policy::none())
            .build()
            .map_err(|e| RedteamError::Probe(format!("cannot build HTTP client: {e}")))?;
        Ok(Self { client, policy })
    }
}

impl HttpProbe for ReqwestProbe {
    fn send<'a>(&'a self, request: &'a ProbeRequest) -> BoxFuture<'a, Result<ProbeResponse, RedteamError>> {
        Box::pin(async move { self.send_checked(request).await })
    }
}

impl ReqwestProbe {
    async fn send_checked(&self, request: &ProbeRequest) -> Result<ProbeResponse, RedteamError> {
        let url = Url::parse(&request.url).map_err(|_| RedteamError::Probe(format!("invalid URL: {}", request.url)))?;
        let host = url.host_str().ok_or_else(|| RedteamError::Probe("URL has no host".into()))?;
        self.policy.check_host(&normalize_host(host))?;

        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|_| RedteamError::Probe(format!("invalid HTTP method: {}", request.method)))?;
        let mut builder = self.client.request(method, url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }

        let start = Instant::now();
        let response = builder.send().await.map_err(|e| RedteamError::Probe(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or_default().to_string()))
            .collect();
        let body = response.text().await.map_err(|e| RedteamError::Probe(e.without_url().to_string()))?;
        let body = body.chars().take(MAX_BODY_CHARS).collect();
        let elapsed = start.elapsed();
        Ok(ProbeResponse { status, headers, body, elapsed })
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    struct AllowHost(&'static str);
    impl TargetPolicy for AllowHost {
        fn check_host(&self, host: &str) -> Result<(), RedteamError> {
            if host == self.0 { Ok(()) } else { Err(RedteamError::HostNotAllowed(host.to_string())) }
        }
        fn allows_destructive(&self, _host: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn sends_a_request_and_captures_status_headers_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello").insert_header("x-test", "1"))
            .mount(&server)
            .await;
        let probe = ReqwestProbe::new(std::sync::Arc::new(AllowHost("127.0.0.1")), Duration::from_secs(5)).unwrap();
        let request =
            ProbeRequest { method: "GET".into(), url: format!("{}/x", server.uri()), headers: vec![], body: None };
        let response = probe.send(&request).await.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "hello");
        assert_eq!(response.header("x-test"), Some("1"));
    }

    #[tokio::test]
    async fn a_host_outside_the_allowlist_is_refused_before_sending() {
        let probe = ReqwestProbe::new(std::sync::Arc::new(AllowHost("127.0.0.1")), Duration::from_secs(5)).unwrap();
        let request =
            ProbeRequest { method: "GET".into(), url: "http://example.com/x".into(), headers: vec![], body: None };
        assert_eq!(probe.send(&request).await, Err(RedteamError::HostNotAllowed("example.com".into())));
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "http://evil.example/"))
            .mount(&server)
            .await;
        let probe = ReqwestProbe::new(std::sync::Arc::new(AllowHost("127.0.0.1")), Duration::from_secs(5)).unwrap();
        let request = ProbeRequest {
            method: "GET".into(),
            url: format!("{}/redirect", server.uri()),
            headers: vec![],
            body: None,
        };
        let response = probe.send(&request).await.unwrap();
        assert_eq!(response.status, 302);
    }
}
