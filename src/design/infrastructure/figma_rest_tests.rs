use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::design::infrastructure::figma_parse::tests::{nodes_body, variables_body};

const TOKEN: &str = "figd_secret-token";

fn source(server: &MockServer) -> FigmaRestSource {
    FigmaRestSource::new(FigmaRestConfig { api_base_url: server.uri(), token: FigmaToken::new(TOKEN) }).unwrap()
}

fn link() -> FigmaLink {
    FigmaLink::parse("https://www.figma.com/design/File1/Shop?node-id=12-345").unwrap()
}

async fn mount_nodes(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/files/File1/nodes"))
        .and(query_param("ids", "12:345"))
        .and(query_param("geometry", "omit"))
        .and(header("X-Figma-Token", TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(nodes_body()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_nodes_with_token_and_api_node_id() {
    let server = MockServer::start().await;
    mount_nodes(&server).await;
    Mock::given(path("/v1/files/File1/variables/local"))
        .respond_with(ResponseTemplate::new(200).set_body_json(variables_body()))
        .mount(&server)
        .await;
    let fetched = source(&server).fetch(&link(), false).await.unwrap();
    assert_eq!(fetched.version, "4242");
    assert_eq!(fetched.spec.texts, vec!["Pay securely now"]);
    assert!(fetched.spec.tokens.iter().any(|t| t.name == "color/brand/primary" && t.value == "#3366ff"));
    assert!(fetched.png.is_none());
    assert!(fetched.notes.is_empty(), "{:?}", fetched.notes);
}

#[tokio::test]
async fn variables_403_falls_back_to_node_values() {
    let server = MockServer::start().await;
    mount_nodes(&server).await;
    Mock::given(path("/v1/files/File1/variables/local"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"status": 403, "err": "Forbidden"})))
        .mount(&server)
        .await;
    let fetched = source(&server).fetch(&link(), false).await.unwrap();
    assert!(fetched.spec.tokens.iter().any(|t| t.name.starts_with("bound variable") && t.value == "16"));
    assert!(fetched.notes[0].contains("variables API not available"));
}

#[tokio::test]
async fn honours_retry_after_on_429_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/files/File1"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(path("/v1/files/File1"))
        .and(query_param("depth", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "77"})))
        .mount(&server)
        .await;
    assert_eq!(source(&server).file_version("File1").await.unwrap(), "77");
}

#[tokio::test]
async fn gives_up_after_two_retries_or_long_retry_after() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/files/File1"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .expect(3)
        .mount(&server)
        .await;
    let err = source(&server).file_version("File1").await.unwrap_err();
    assert!(matches!(&err, DesignError::Unavailable(m) if m.contains("after 2 retries")), "{err}");

    let slow = MockServer::start().await;
    Mock::given(path("/v1/files/File1"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "3600"))
        .expect(1)
        .mount(&slow)
        .await;
    assert!(matches!(source(&slow).file_version("File1").await, Err(DesignError::Unavailable(_))));
}

#[tokio::test]
async fn downloads_png_from_allowed_host_without_token() {
    let server = MockServer::start().await;
    mount_nodes(&server).await;
    Mock::given(path("/v1/files/File1/variables/local")).respond_with(ResponseTemplate::new(403)).mount(&server).await;
    let image_url = format!("{}/render/frame.png", server.uri());
    Mock::given(path("/v1/images/File1"))
        .and(query_param("ids", "12:345"))
        .and(query_param("format", "png"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"err": null, "images": {"12:345": image_url}})))
        .mount(&server)
        .await;
    let png = [&PNG_MAGIC[..], b"rest-of-image"].concat();
    Mock::given(path("/render/frame.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png.clone()))
        .mount(&server)
        .await;
    let fetched = source(&server).fetch(&link(), true).await.unwrap();
    assert_eq!(fetched.png.as_deref(), Some(&png[..]));
    let requests = server.received_requests().await.unwrap();
    let download = requests.iter().find(|r| r.url.path() == "/render/frame.png").unwrap();
    assert!(!download.headers.contains_key("x-figma-token"), "token must not reach the image host");
}

#[tokio::test]
async fn rejects_image_hosts_outside_the_allow_list_and_non_png() {
    let server = MockServer::start().await;
    let source = source(&server);
    Mock::given(path("/v1/images/File1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"images": {"12:345": "https://evil.example.com/x.png"}})),
        )
        .mount(&server)
        .await;
    let err = source.png(&link()).await.unwrap_err();
    assert!(err.to_string().contains("not an allowed Figma host"), "{err}");

    let allowed = |u: &str| source.is_allowed_image_host(&Url::parse(u).unwrap());
    assert!(allowed("https://figma-alpha-api.s3.us-west-2.amazonaws.com/images/abc"));
    assert!(allowed("https://s3-alpha.figma.com/x.png"));
    assert!(!allowed("http://s3-alpha.figma.com/x.png"));
    assert!(!allowed("https://figma.com.evil.io/x.png"));
    assert!(!allowed("https://evil-bucket.s3.amazonaws.com/x.png"));

    let other = MockServer::start().await;
    Mock::given(path("/v1/images/File1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"images": {"12:345": format!("{}/x", other.uri())}})),
        )
        .mount(&other)
        .await;
    Mock::given(path("/x")).respond_with(ResponseTemplate::new(200).set_body_string("<html>")).mount(&other).await;
    let err = super::FigmaRestSource::new(FigmaRestConfig { api_base_url: other.uri(), token: FigmaToken::new(TOKEN) })
        .unwrap()
        .png(&link())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a PNG"), "{err}");
}

#[tokio::test]
async fn oversized_image_is_skipped_with_note() {
    let server = MockServer::start().await;
    mount_nodes(&server).await;
    Mock::given(path("/v1/files/File1/variables/local")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
    Mock::given(path("/v1/images/File1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"images": {"12:345": format!("{}/big.png", server.uri())}})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/big.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; MAX_IMAGE_BYTES + 1]))
        .mount(&server)
        .await;
    let fetched = source(&server).fetch(&link(), true).await.unwrap();
    assert!(fetched.png.is_none());
    assert!(fetched.notes.iter().any(|n| n.contains("larger than 5 MB")), "{:?}", fetched.notes);
}

#[tokio::test]
async fn maps_errors_and_never_leaks_the_token() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/files/File1/nodes")).respond_with(ResponseTemplate::new(403)).mount(&server).await;
    Mock::given(path("/v1/files/File2"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "https://x.io"))
        .mount(&server)
        .await;
    Mock::given(path("/v1/files/File3")).respond_with(ResponseTemplate::new(503)).mount(&server).await;
    Mock::given(path("/v1/files/File4"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let source = source(&server);
    let err = source.fetch(&link(), false).await.unwrap_err();
    assert!(matches!(err, DesignError::Unauthorized(_)), "{err}");
    assert!(matches!(source.file_version("File2").await, Err(DesignError::Invalid(m)) if m.contains("redirect")));
    assert!(matches!(source.file_version("File3").await, Err(DesignError::Unavailable(_))));
    let invalid = source.file_version("File4").await.unwrap_err();
    assert!(matches!(invalid, DesignError::Invalid(_)));
    assert!(matches!(source.file_version("../x").await, Err(DesignError::Config(_))));
    assert!(matches!(source.file_version("File5").await, Err(DesignError::NotFound(_))));
    for err in [err, invalid] {
        assert!(!err.to_string().contains(TOKEN));
    }
    assert!(!format!("{source:?}").contains(TOKEN));
    assert_eq!(format!("{:?}", FigmaToken::new(TOKEN)), "FigmaToken(***)");
}

#[tokio::test]
async fn unreachable_server_is_unavailable() {
    let source = FigmaRestSource::new(FigmaRestConfig {
        api_base_url: "http://127.0.0.1:1".into(),
        token: FigmaToken::new(TOKEN),
    })
    .unwrap();
    assert!(matches!(source.file_version("File1").await, Err(DesignError::Unavailable(_))));
}

#[test]
fn validates_configuration() {
    let build = |url: &str, token: &str| {
        FigmaRestSource::new(FigmaRestConfig { api_base_url: url.into(), token: FigmaToken::new(token) })
    };
    assert!(build("https://api.figma.com", TOKEN).is_ok());
    assert!(matches!(build("http://api.figma.com", TOKEN), Err(DesignError::Config(_))));
    assert!(matches!(build("https://u:p@api.figma.com", TOKEN), Err(DesignError::Config(_))));
    assert!(matches!(build("not a url", TOKEN), Err(DesignError::Config(_))));
    assert!(matches!(build("https://api.figma.com", "  "), Err(DesignError::Config(_))));
    let prefixed = build("https://proxy.example.com/figma", TOKEN).unwrap();
    assert_eq!(prefixed.url("v1/files/K", &[]).unwrap().as_str(), "https://proxy.example.com/figma/v1/files/K");
}
