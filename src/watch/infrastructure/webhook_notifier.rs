//! Notifier posting `{"text": "..."}` to a webhook (Slack incoming-webhook compatible).

use std::time::Duration;

use reqwest::{Client, Url, redirect};
use serde::Serialize;

use crate::watch::application::ports::{Notifier, WatchError};

const TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

#[derive(Serialize)]
struct Payload<'a> {
    text: &'a str,
}

/// Best effort: failures are logged on stderr (without the URL, which usually embeds a
/// secret token) and never propagated.
#[derive(Debug, Clone)]
pub struct WebhookNotifier {
    client: Client,
    url: Url,
}

impl WebhookNotifier {
    pub fn new(url: &str) -> Result<Self, WatchError> {
        let url = validate_webhook_url(url)?;
        let client = Client::builder()
            .timeout(TIMEOUT)
            // A redirect could downgrade to plain HTTP or leave the allowed host.
            .redirect(redirect::Policy::none())
            .build()
            .map_err(|e| WatchError::Config(format!("cannot build HTTP client: {e}")))?;
        Ok(Self { client, url })
    }
}

/// Accepts `https://` URLs, and `http://` only for the local host.
pub fn validate_webhook_url(raw: &str) -> Result<Url, WatchError> {
    let invalid = |why: &str| WatchError::Config(format!("webhook URL {why}"));
    let url = Url::parse(raw.trim()).map_err(|_| invalid("is not a valid URL"))?;
    let host = url.host_str().ok_or_else(|| invalid("has no host"))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if LOCAL_HOSTS.contains(&host) => Ok(url),
        _ => Err(invalid("must use https (plain http is allowed only for localhost)")),
    }
}

impl Notifier for WebhookNotifier {
    async fn notify(&self, title: &str, body: &str) {
        let text = if body.is_empty() { title.to_owned() } else { format!("{title}\n{body}") };
        let sent = self.client.post(self.url.clone()).json(&Payload { text: &text }).send().await;
        match sent {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => eprintln!("qgate watch: webhook returned HTTP {}", response.status()),
            Err(error) => eprintln!("qgate watch: webhook failed: {}", error.without_url()),
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn accepts_https_and_local_http() {
        for url in [
            "https://hooks.slack.com/services/T/B/x",
            "http://localhost:8080/hook",
            "http://127.0.0.1/hook",
            "http://[::1]:9000/",
        ] {
            assert!(validate_webhook_url(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn rejects_insecure_or_invalid_urls() {
        for url in ["http://example.com/hook", "ftp://localhost/x", "not a url", "file:///etc/passwd"] {
            assert!(matches!(validate_webhook_url(url), Err(WatchError::Config(_))), "{url}");
        }
    }

    #[tokio::test]
    async fn posts_slack_compatible_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(body_json(serde_json::json!({ "text": "title\nbody" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let notifier = WebhookNotifier::new(&format!("{}/hook", server.uri())).unwrap();
        notifier.notify("title", "body").await;
    }

    #[tokio::test]
    async fn server_error_and_unreachable_host_do_not_fail() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(500)).expect(1).mount(&server).await;
        WebhookNotifier::new(&server.uri()).unwrap().notify("t", "").await;

        // Port 9 (discard) on loopback is normally closed: connection refused.
        WebhookNotifier::new("http://127.0.0.1:9/").unwrap().notify("t", "b").await;
    }
}
