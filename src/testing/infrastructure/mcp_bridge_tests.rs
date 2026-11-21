use serde_json::{Value, json};

use super::*;

fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "echo".into(),
        description: "Echoes".into(),
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    }]
}

fn spawn_driver(mut calls: mpsc::Receiver<ToolCall>) {
    tokio::spawn(async move {
        while let Some(call) = calls.recv().await {
            let output = if call.name == "fail" {
                ToolOutput::error("boom")
            } else {
                ToolOutput::ok(format!("echo:{}", call.input))
            };
            let _ = call.reply.send(output);
        }
    });
}

#[tokio::test]
async fn forward_relays_the_drivers_answer() {
    let (tx, rx) = mpsc::channel(4);
    spawn_driver(rx);
    let bridge = ToolBridge::new(&specs(), tx);
    let out = bridge.forward("echo", json!({"a": 1})).await;
    assert_eq!(out, ToolOutput::ok(r#"echo:{"a":1}"#));
}

#[tokio::test]
async fn forward_rejects_unknown_tools_without_asking_the_driver() {
    let (tx, mut rx) = mpsc::channel(4);
    let bridge = ToolBridge::new(&specs(), tx);
    let out = bridge.forward("rm_rf", json!({})).await;
    assert!(out.is_error && out.content.contains("unknown tool"));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn forward_reports_a_finished_session() {
    let (tx, rx) = mpsc::channel(4);
    drop(rx);
    let out = ToolBridge::new(&specs(), tx).forward("echo", json!({})).await;
    assert!(out.is_error && out.content.contains("ended"));
}

#[tokio::test]
async fn forward_reports_a_driver_that_drops_the_reply() {
    let (tx, mut rx) = mpsc::channel(4);
    tokio::spawn(async move {
        let _ = rx.recv().await;
    });
    let out = ToolBridge::new(&specs(), tx).forward("echo", json!({})).await;
    assert!(out.is_error && out.content.contains("ended"));
}

const ACCEPT: &str = "application/json, text/event-stream";

async fn rpc(http: &reqwest::Client, url: &str, session: Option<&str>, body: Value) -> (Option<String>, Value) {
    let mut request = http.post(url).header("Accept", ACCEPT).json(&body);
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session).header("MCP-Protocol-Version", "2025-06-18");
    }
    let response = request.send().await.unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let session = response.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
    let text = response.text().await.unwrap();
    let data = text.lines().rev().find_map(|l| l.strip_prefix("data:")).map_or(text.as_str(), str::trim);
    (session, serde_json::from_str(data).unwrap_or(Value::Null))
}

#[tokio::test]
async fn serves_the_tools_over_mcp_http() {
    let (tx, rx) = mpsc::channel(4);
    spawn_driver(rx);
    let server = RunningServer::start(ToolBridge::new(&specs(), tx)).await.unwrap();
    let config = server.mcp_config();
    let url = config["mcpServers"][SERVER_NAME]["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("http://127.0.0.1:") && url.contains("/mcp/"));
    let http = reqwest::Client::new();

    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}});
    let (session, reply) = rpc(&http, &url, None, init).await;
    let session = session.expect("session id");
    assert!(reply["result"]["capabilities"]["tools"].is_object(), "{reply}");
    http.post(&url)
        .header("Accept", ACCEPT)
        .header("Mcp-Session-Id", &session)
        .header("MCP-Protocol-Version", "2025-06-18")
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();

    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let (_, listed) = rpc(&http, &url, Some(&session), list).await;
    assert_eq!(listed["result"]["tools"][0]["name"], "echo");

    let call = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "echo", "arguments": {"k": "v"}}});
    let (_, called) = rpc(&http, &url, Some(&session), call).await;
    assert_eq!(called["result"]["content"][0]["text"], r#"echo:{"k":"v"}"#);
    assert_eq!(called["result"]["isError"], false);

    let bad = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "fail"}});
    let (_, failed) = rpc(&http, &url, Some(&session), bad).await;
    assert_eq!(failed["result"]["isError"], true);
}

#[tokio::test]
async fn a_wrong_path_is_not_served_and_the_server_stops_on_drop() {
    let (tx, _rx) = mpsc::channel(1);
    let server = RunningServer::start(ToolBridge::new(&specs(), tx)).await.unwrap();
    let url = server.mcp_config()["mcpServers"][SERVER_NAME]["url"].as_str().unwrap().to_string();
    let base = url.split("/mcp/").next().unwrap().to_string();
    let http = reqwest::Client::new();
    let guessed = http.post(format!("{base}/mcp/guess")).send().await.unwrap();
    assert_eq!(guessed.status(), 404);
    drop(server);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(http.post(&url).send().await.is_err(), "server must be gone");
}
