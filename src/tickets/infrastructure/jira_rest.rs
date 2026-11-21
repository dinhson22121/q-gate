//! Jira Cloud / Server REST adapter (`GET /rest/api/3/issue/{key}`), the default ticket source.

use std::time::Duration;

use reqwest::{StatusCode, Url};
use serde_json::Value;

use crate::tickets::application::{TicketError, TicketSource};
use crate::tickets::domain::{
    ParentRef, Ticket, TicketKey, extract_figma_links, merge_links, parse_acceptance_criteria, parse_criteria_list,
};

use super::adf::{all_strings, field_to_text};
use super::secret::Secret;

/// Per-request timeout for Jira calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Fields always requested from Jira.
const BASE_FIELDS: &str = "summary,description,status,issuetype,parent,updated";
/// Field requested by the cheap freshness probe.
const UPDATED_FIELD: &str = "updated";
/// Hosts allowed over plain HTTP (local test servers only).
const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

/// Connection settings for [`JiraRestSource`].
#[derive(Debug, Clone)]
pub struct JiraRestConfig {
    /// e.g. `https://acme.atlassian.net` (https required; http only for localhost).
    pub base_url: String,
    pub email: String,
    pub token: Secret,
    /// Custom field holding acceptance criteria, e.g. `customfield_10042`. When set and non-empty
    /// its items are the criteria; otherwise the description is parsed.
    pub ac_field: Option<String>,
}

/// Fetches tickets from the Jira REST API v3 using basic auth (email + API token).
#[derive(Debug, Clone)]
pub struct JiraRestSource {
    client: reqwest::Client,
    base_url: String,
    email: String,
    token: Secret,
    ac_field: Option<String>,
}

impl JiraRestSource {
    /// Validates the configuration and builds the HTTP client.
    pub fn new(config: JiraRestConfig) -> Result<Self, TicketError> {
        let base_url = validate_base_url(&config.base_url)?;
        if config.email.trim().is_empty() || config.token.expose().trim().is_empty() {
            return Err(TicketError::Config("Jira email and API token are required".into()));
        }
        let ac_field = config.ac_field.map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
        if let Some(field) = &ac_field {
            validate_field_name(field)?;
        }
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // Never follow redirects: the credentials must only ever reach the configured host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| TicketError::Config(format!("cannot build HTTP client: {e}")))?;
        Ok(Self { client, base_url, email: config.email.trim().to_string(), token: config.token, ac_field })
    }

    fn issue_url(&self, key: &TicketKey) -> String {
        let fields = match &self.ac_field {
            Some(field) => format!("{BASE_FIELDS},{field}"),
            None => BASE_FIELDS.to_string(),
        };
        self.issue_url_with(key, &fields)
    }

    fn issue_url_with(&self, key: &TicketKey, fields: &str) -> String {
        format!("{}/rest/api/3/issue/{key}?fields={fields}", self.base_url)
    }

    async fn get_issue(&self, key: &TicketKey) -> Result<Value, TicketError> {
        self.get_json(&self.issue_url(key), key).await
    }

    /// Figma links of the issue's remote links (`object.url`). Best effort: a failure only loses links.
    async fn remote_figma_links(&self, key: &TicketKey) -> Vec<String> {
        let url = format!("{}/rest/api/3/issue/{key}/remotelink", self.base_url);
        match self.get_json(&url, key).await {
            Ok(links) => remote_link_urls(&links).iter().flat_map(|u| extract_figma_links(u)).collect(),
            // Old Jira versions or disabled remote links: nothing to read.
            Err(TicketError::NotFound(_)) => Vec::new(),
            Err(error) => {
                eprintln!("qgate tickets: remote links of {key} not read: {error}");
                Vec::new()
            }
        }
    }

    async fn get_json(&self, url: &str, key: &TicketKey) -> Result<Value, TicketError> {
        let response = self
            .client
            .get(url)
            .basic_auth(&self.email, Some(self.token.expose()))
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(transport_error)?;
        check_status(response.status(), key)?;
        response.json().await.map_err(|e| TicketError::Invalid(format!("issue JSON: {}", e.without_url())))
    }
}

impl TicketSource for JiraRestSource {
    async fn fetch(&self, key: &TicketKey) -> Result<Ticket, TicketError> {
        let issue = self.get_issue(key).await?;
        let mut ticket = map_issue(key, &issue, self.ac_field.as_deref(), &format!("{}/browse/{key}", self.base_url))?;
        merge_links(&mut ticket.design_links, self.remote_figma_links(key).await);
        Ok(ticket)
    }

    async fn fetch_updated(&self, key: &TicketKey) -> Result<Option<String>, TicketError> {
        let issue = self.get_json(&self.issue_url_with(key, UPDATED_FIELD), key).await?;
        Ok(issue.pointer("/fields/updated").and_then(Value::as_str).map(str::to_string))
    }
}

/// `[{"object": {"url": ...}}, ...]` → the URLs.
fn remote_link_urls(links: &Value) -> Vec<String> {
    links
        .as_array()
        .map(|items| {
            items.iter().filter_map(|l| l.pointer("/object/url").and_then(Value::as_str)).map(str::to_string).collect()
        })
        .unwrap_or_default()
}

/// Requires https (http only for localhost) and strips the trailing slash.
fn validate_base_url(raw: &str) -> Result<String, TicketError> {
    let url = Url::parse(raw.trim()).map_err(|e| TicketError::Config(format!("invalid Jira base URL: {e}")))?;
    let host = url.host_str().unwrap_or_default();
    let is_local_http = url.scheme() == "http" && LOCAL_HOSTS.contains(&host);
    if url.scheme() != "https" && !is_local_http {
        return Err(TicketError::Config("Jira base URL must use https".into()));
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(TicketError::Config("Jira base URL must not contain credentials, query or fragment".into()));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Custom field ids are interpolated into the query string, so only allow `[A-Za-z0-9_]`.
fn validate_field_name(field: &str) -> Result<(), TicketError> {
    if field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(())
    } else {
        Err(TicketError::Config(format!("invalid Jira field id {field:?}")))
    }
}

fn transport_error(error: reqwest::Error) -> TicketError {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    TicketError::Unavailable(format!("Jira {kind}: {}", error.without_url()))
}

fn check_status(status: StatusCode, key: &TicketKey) -> Result<(), TicketError> {
    match status {
        s if s.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err(TicketError::Unauthorized(format!("Jira returned {status}; check email and API token")))
        }
        StatusCode::NOT_FOUND => Err(TicketError::NotFound(key.to_string())),
        StatusCode::TOO_MANY_REQUESTS => Err(TicketError::Unavailable(format!("Jira returned {status}"))),
        s if s.is_server_error() => Err(TicketError::Unavailable(format!("Jira returned {status}"))),
        s if s.is_redirection() => Err(TicketError::Invalid(format!(
            "Jira returned an unexpected redirect ({status}); redirects are not followed, check jira.base_url"
        ))),
        _ => Err(TicketError::Invalid(format!("Jira returned {status}"))),
    }
}

/// Maps a Jira issue JSON (`{"fields": {...}}`) to a ticket.
fn map_issue(key: &TicketKey, issue: &Value, ac_field: Option<&str>, url: &str) -> Result<Ticket, TicketError> {
    let fields = issue
        .get("fields")
        .filter(|f| f.is_object())
        .ok_or_else(|| TicketError::Invalid("issue has no fields object".into()))?;
    let title = fields.get("summary").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let status = fields.pointer("/status/name").and_then(Value::as_str).map(str::to_string);
    let from_field = ac_field
        .and_then(|name| fields.get(name))
        .and_then(field_to_text)
        .map(|text| parse_criteria_list(&text))
        .filter(|criteria| !criteria.is_empty());
    let raw_description = fields.get("description").unwrap_or(&Value::Null);
    let description = field_to_text(raw_description).unwrap_or_default();
    let criteria = from_field.unwrap_or_else(|| parse_acceptance_criteria(&description));
    let parent =
        fields.pointer("/parent/key").and_then(Value::as_str).and_then(|k| TicketKey::parse(k).ok()).map(|k| {
            ParentRef {
                key: k,
                issue_type: fields.pointer("/parent/fields/issuetype/name").and_then(Value::as_str).map(str::to_string),
            }
        });
    Ok(Ticket {
        url: Some(url.to_string()),
        status,
        criteria,
        issue_type: fields.pointer("/issuetype/name").and_then(Value::as_str).map(str::to_string),
        parent,
        design_links: extract_figma_links(&all_strings(raw_description)),
        description,
        updated: fields.get("updated").and_then(Value::as_str).map(str::to_string),
        ..Ticket::new(key.clone(), title)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// base64("me@example.com:tok-123")
    const EXPECTED_AUTH: &str = "Basic bWVAZXhhbXBsZS5jb206dG9rLTEyMw==";

    fn config(base_url: &str, ac_field: Option<&str>) -> JiraRestConfig {
        JiraRestConfig {
            base_url: base_url.into(),
            email: "me@example.com".into(),
            token: Secret::new("tok-123"),
            ac_field: ac_field.map(str::to_string),
        }
    }

    fn key() -> TicketKey {
        TicketKey::parse("ABC-123").unwrap()
    }

    fn adf_description() -> Value {
        json!({"type": "doc", "version": 1, "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "Users want exports."}]},
            {"type": "heading", "attrs": {"level": 3}, "content": [{"type": "text", "text": "Acceptance Criteria"}]},
            {"type": "bulletList", "content": [
                {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "CSV export button"}]}]},
                {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Export is audited"}]}]}
            ]}
        ]})
    }

    async fn serve(status: u16, body: Value, fields: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ABC-123"))
            .and(query_param("fields", fields))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn fetches_ticket_with_criteria_from_adf_description() {
        let body = json!({"key": "ABC-123", "fields": {
            "summary": "CSV export", "status": {"name": "In Progress"}, "description": adf_description()
        }});
        let server = serve(200, body, BASE_FIELDS).await;
        let source = JiraRestSource::new(config(&format!("{}/", server.uri()), None)).unwrap();

        let ticket = source.fetch(&key()).await.unwrap();

        assert_eq!(ticket.title, "CSV export");
        assert_eq!(ticket.status.as_deref(), Some("In Progress"));
        assert_eq!(ticket.url, Some(format!("{}/browse/ABC-123", server.uri())));
        assert_eq!(ticket.criteria, vec!["CSV export button", "Export is audited"]);
    }

    #[tokio::test]
    async fn custom_field_criteria_take_precedence() {
        let body = json!({"fields": {
            "summary": "CSV export", "status": {"name": "Done"}, "description": adf_description(),
            "customfield_10042": {"type": "doc", "version": 1, "content": [
                {"type": "orderedList", "content": [
                    {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "From custom field"}]}]}
                ]}
            ]}
        }});
        let server = serve(200, body, &format!("{BASE_FIELDS},customfield_10042")).await;
        let source = JiraRestSource::new(config(&server.uri(), Some(" customfield_10042 "))).unwrap();

        assert_eq!(source.fetch(&key()).await.unwrap().criteria, vec!["From custom field"]);
    }

    #[tokio::test]
    async fn empty_custom_field_falls_back_to_string_description() {
        let body = json!({"fields": {
            "summary": "x", "status": null, "customfield_1": null,
            "description": "h3. Acceptance Criteria\n* wiki item"
        }});
        let server = serve(200, body, &format!("{BASE_FIELDS},customfield_1")).await;
        let source = JiraRestSource::new(config(&server.uri(), Some("customfield_1"))).unwrap();

        let ticket = source.fetch(&key()).await.unwrap();
        assert_eq!(ticket.criteria, vec!["wiki item"]);
        assert_eq!(ticket.status, None);
    }

    #[tokio::test]
    async fn maps_http_errors() {
        let cases = [
            (401, TicketError::Unauthorized(String::new())),
            (403, TicketError::Unauthorized(String::new())),
            (404, TicketError::NotFound("ABC-123".into())),
            (500, TicketError::Unavailable(String::new())),
            (429, TicketError::Unavailable(String::new())),
            (400, TicketError::Invalid(String::new())),
        ];
        for (status, expected) in cases {
            let server = serve(status, json!({"errorMessages": ["nope"]}), BASE_FIELDS).await;
            let source = JiraRestSource::new(config(&server.uri(), None)).unwrap();
            let err = source.fetch(&key()).await.unwrap_err();
            assert_eq!(std::mem::discriminant(&err), std::mem::discriminant(&expected), "status {status}: {err}");
            assert!(!err.to_string().contains("tok-123"));
        }
    }

    #[tokio::test]
    async fn redirect_is_invalid_and_not_followed() {
        let target = MockServer::start().await;
        let server = MockServer::start().await;
        let location = format!("{}/rest/api/3/issue/ABC-123?fields={BASE_FIELDS}", target.uri());
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location.as_str()))
            .expect(1)
            .mount(&server)
            .await;
        let source = JiraRestSource::new(config(&server.uri(), None)).unwrap();

        let err = source.fetch(&key()).await.unwrap_err();

        assert!(matches!(&err, TicketError::Invalid(m) if m.contains("redirect")), "{err}");
        assert!(!err.to_string().contains("tok-123"));
        assert!(target.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_json_and_missing_fields_are_invalid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .mount(&server)
            .await;
        let source = JiraRestSource::new(config(&server.uri(), None)).unwrap();
        assert!(matches!(source.fetch(&key()).await, Err(TicketError::Invalid(_))));

        let missing = map_issue(&key(), &json!({"errorMessages": []}), None, "u");
        assert!(matches!(missing, Err(TicketError::Invalid(_))));
    }

    #[tokio::test]
    async fn unreachable_server_is_unavailable() {
        // Port 9 (discard) is closed on developer machines and CI runners.
        let source = JiraRestSource::new(config("http://127.0.0.1:9", None)).unwrap();
        assert!(matches!(source.fetch(&key()).await, Err(TicketError::Unavailable(_))));
    }

    #[test]
    fn validates_configuration() {
        let bad_urls =
            ["http://acme.atlassian.net", "ftp://x", "not a url", "https://u:p@acme.net", "https://acme.net?x=1"];
        for url in bad_urls {
            assert!(matches!(JiraRestSource::new(config(url, None)), Err(TicketError::Config(_))), "{url}");
        }
        assert!(matches!(JiraRestSource::new(config("https://a.net", Some("cf&x=1"))), Err(TicketError::Config(_))));
        let mut no_token = config("https://a.net", None);
        no_token.token = Secret::new(" ");
        assert!(matches!(JiraRestSource::new(no_token), Err(TicketError::Config(_))));
        let source = JiraRestSource::new(config("https://acme.atlassian.net/", Some(""))).unwrap();
        assert_eq!(
            source.issue_url(&key()),
            "https://acme.atlassian.net/rest/api/3/issue/ABC-123?fields=summary,description,status,issuetype,parent,updated"
        );
        assert!(JiraRestSource::new(config("http://127.0.0.1:9", None)).is_ok());
    }

    #[test]
    fn debug_output_redacts_token() {
        let source = JiraRestSource::new(config("https://a.net", None)).unwrap();
        assert!(!format!("{source:?}").contains("tok-123"));
    }

    async fn mount_issue(server: &MockServer, issue: &str, fields: Value) {
        Mock::given(method("GET"))
            .and(path(format!("/rest/api/3/issue/{issue}")))
            .and(query_param("fields", BASE_FIELDS))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": issue, "fields": fields})))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_remote_links(server: &MockServer, issue: &str, status: u16, body: Value) {
        Mock::given(method("GET"))
            .and(path(format!("/rest/api/3/issue/{issue}/remotelink")))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn subtask_parent_story_walk_merges_criteria_and_figma_links() {
        use crate::tickets::application::{LoadOptions, LoadTicketContext};

        let server = MockServer::start().await;
        let figma_mark = json!({"type": "text", "text": "frame", "marks": [{"type": "link", "attrs": {
            "href": "https://www.figma.com/design/SubKey/Cart?node-id=1-2"}}]});
        mount_issue(&server, "SHOP-8", json!({
            "summary": "Cart API", "status": {"name": "In Progress"}, "issuetype": {"name": "Sub-task"},
            "updated": "2026-11-20T10:00:00.000+0000",
            "parent": {"key": "SHOP-7", "fields": {"summary": "Cart", "issuetype": {"name": "Story"}}},
            "description": {"type": "doc", "version": 1, "content": [
                {"type": "paragraph", "content": [figma_mark]},
                {"type": "heading", "attrs": {"level": 3}, "content": [{"type": "text", "text": "Acceptance Criteria"}]},
                {"type": "bulletList", "content": [
                    {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "API returns totals"}]}]}
                ]}
            ]}
        }))
        .await;
        mount_issue(&server, "SHOP-7", json!({
            "summary": "Cart", "status": {"name": "To Do"}, "issuetype": {"name": "Story"},
            "parent": {"key": "SHOP-1", "fields": {"issuetype": {"name": "Epic"}}},
            "description": "Shoppers see a cart.\n\nh3. Acceptance Criteria\n* Totals include tax\n* Empty cart message"
        }))
        .await;
        mount_remote_links(&server, "SHOP-8", 404, json!({})).await;
        mount_remote_links(
            &server,
            "SHOP-7",
            200,
            json!([
                {"object": {"url": "https://www.figma.com/design/StoryKey/Checkout?node-id=12-345", "title": "Design"}},
                {"object": {"url": "https://www.figma.com/board/Brainstorm/x"}},
                {"object": {"title": "no url"}}
            ]),
        )
        .await;
        let source = JiraRestSource::new(config(&server.uri(), None)).unwrap();

        let loaded = LoadTicketContext::new(source, LoadOptions::default())
            .execute(&[TicketKey::parse("SHOP-8").unwrap()])
            .await
            .unwrap();

        let ids: Vec<_> = loaded.requirements.criteria.iter().map(|c| (c.id.as_str(), c.text.as_str())).collect();
        assert_eq!(
            ids,
            vec![
                ("SHOP-8/AC-1", "API returns totals"),
                ("SHOP-7/AC-1", "Totals include tax"),
                ("SHOP-7/AC-2", "Empty cart message")
            ]
        );
        assert_eq!(loaded.context.keys, vec!["SHOP-8", "SHOP-7"]);
        assert_eq!(
            loaded.context.design_links,
            vec![
                "https://www.figma.com/design/StoryKey/Checkout?node-id=12-345",
                "https://www.figma.com/design/SubKey/Cart?node-id=1-2"
            ]
        );
        assert!(loaded.context.description.starts_with("[SHOP-8] Cart API\nframe"), "{}", loaded.context.description);
        assert!(loaded.context.description.contains("[SHOP-7] Cart\nShoppers see a cart."));
        assert!(loaded.notes.is_empty(), "{:?}", loaded.notes);
    }

    #[tokio::test]
    async fn remote_link_errors_only_lose_links() {
        let server = MockServer::start().await;
        mount_issue(&server, "ABC-123", json!({"summary": "x", "issuetype": {"name": "Task"}})).await;
        mount_remote_links(&server, "ABC-123", 500, json!({})).await;
        let source = JiraRestSource::new(config(&server.uri(), None)).unwrap();

        let ticket = source.fetch(&key()).await.unwrap();

        assert_eq!(ticket.issue_type.as_deref(), Some("Task"));
        assert!(ticket.design_links.is_empty() && ticket.parent.is_none() && ticket.updated.is_none());
    }

    #[tokio::test]
    async fn fetch_updated_requests_only_the_updated_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ABC-123"))
            .and(query_param("fields", UPDATED_FIELD))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"fields": {"updated": "2026-11-20T10:00:00.000+0000"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let source = JiraRestSource::new(config(&server.uri(), None)).unwrap();

        assert_eq!(source.fetch_updated(&key()).await.unwrap().as_deref(), Some("2026-11-20T10:00:00.000+0000"));
        let missing = TicketKey::parse("ABC-9").unwrap();
        assert_eq!(source.fetch_updated(&missing).await, Err(TicketError::NotFound("ABC-9".into())));
    }
}
