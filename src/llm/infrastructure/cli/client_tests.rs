use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::*;

const SUCCESS: &str = include_str!("fixtures/structured_success.json");
const RATE_LIMITED: &str = include_str!("fixtures/rate_limited_synthetic.json");

#[derive(Debug, Deserialize, PartialEq)]
struct Named {
    name: String,
    note: String,
}

fn schema() -> Value {
    json!({"type":"object","properties":{"name":{"type":"string"},"note":{"type":"string"}},"required":["name","note"]})
}

fn fake_claude(dir: &tempfile::TempDir, body: &str) -> CliClient {
    let path: PathBuf = dir.path().join("claude");
    std::fs::write(&path, format!("#!/bin/sh\ncat >/dev/null\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    CliClient::new(CliConfig { claude_bin: path, timeout: Duration::from_secs(10), ..CliConfig::default() }).unwrap()
}

fn emit(dir: &tempfile::TempDir, name: &str, json: &str) -> String {
    let file = dir.path().join(name);
    std::fs::write(&file, json).unwrap();
    format!("cat {}", file.display())
}

#[tokio::test]
async fn structured_parses_real_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let client = fake_claude(&dir, &emit(&dir, "ok.json", SUCCESS));

    let out = client.structured::<Named>("sys", "prompt", &schema()).await.unwrap();

    assert_eq!(out.value.name, "qgate");
    assert_eq!(out.usage.output_tokens, 134);
    assert!(out.cost_usd > 0.0);
}

#[tokio::test]
async fn structured_reads_json_text_when_no_structured_output() {
    let dir = tempfile::tempdir().unwrap();
    let envelope = json!({"is_error": false, "result": "{\"name\":\"a\",\"note\":\"b\"}"}).to_string();
    let client = fake_claude(&dir, &emit(&dir, "text.json", &envelope));

    let out = client.structured::<Named>("sys", "prompt", &schema()).await.unwrap();

    assert_eq!(out.value, Named { name: "a".into(), note: "b".into() });
}

#[tokio::test]
async fn structured_retries_once_on_invalid_answer_then_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let bad = json!({"is_error": false, "structured_output": {"wrong": 1}}).to_string();
    let good = emit(&dir, "good.json", SUCCESS);
    let bad_cmd = emit(&dir, "bad.json", &bad);
    let marker = dir.path().join("seen");
    let body = format!("if [ -e {m} ]; then {good}; else touch {m}; {bad_cmd}; fi", m = marker.display());
    let client = fake_claude(&dir, &body);

    let out = client.structured::<Named>("sys", "prompt", &schema()).await.unwrap();

    assert_eq!(out.value.name, "qgate");
}

#[tokio::test]
async fn structured_gives_up_after_one_retry() {
    let dir = tempfile::tempdir().unwrap();
    let bad = json!({"is_error": false, "structured_output": {"wrong": 1}}).to_string();
    let client = fake_claude(&dir, &emit(&dir, "bad.json", &bad));

    let err = client.structured::<Named>("sys", "prompt", &schema()).await.unwrap_err();

    assert!(matches!(err, LlmError::InvalidResponse(_)));
}

#[tokio::test]
async fn rate_limit_is_returned_with_reset_time_and_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let client = fake_claude(&dir, &format!("{}; exit 1", emit(&dir, "rl.json", RATE_LIMITED)));

    let err = client.structured::<Named>("sys", "prompt", &schema()).await.unwrap_err();

    assert!(matches!(err, LlmError::RateLimited { resets_at: Some(t), .. } if t.starts_with("4:30pm")));
}

#[tokio::test]
async fn preflight_propagates_rate_limit_and_passes_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let limited = fake_claude(&dir, &emit(&dir, "rl.json", RATE_LIMITED));
    assert!(matches!(limited.preflight().await, Err(LlmError::RateLimited { .. })));

    let ok_dir = tempfile::tempdir().unwrap();
    let ok = fake_claude(&ok_dir, &emit(&ok_dir, "ok.json", r#"{"is_error":false,"result":"ok"}"#));
    assert_eq!(ok.preflight().await, Ok(()));
}

#[tokio::test]
async fn non_json_output_becomes_unavailable_or_rate_limited_from_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let broken = fake_claude(&dir, "echo 'Error: boom' >&2; exit 1");
    assert!(matches!(broken.preflight().await, Err(LlmError::Unavailable(m)) if m.contains("boom")));

    let dir2 = tempfile::tempdir().unwrap();
    let limited = fake_claude(&dir2, "echo 'Claude AI usage limit reached|1760000000' >&2; exit 1");
    assert!(matches!(limited.preflight().await, Err(LlmError::RateLimited { resets_at: Some(_), .. })));
}

#[tokio::test]
async fn missing_binary_and_timeout_are_unavailable() {
    let missing =
        CliClient::new(CliConfig { claude_bin: "/nonexistent/claude".into(), ..CliConfig::default() }).unwrap();
    assert!(matches!(missing.preflight().await, Err(LlmError::Unavailable(m)) if m.contains("cannot start")));

    let dir = tempfile::tempdir().unwrap();
    let mut slow = fake_claude(&dir, "sleep 5");
    slow.config.timeout = Duration::from_millis(100);
    assert!(matches!(slow.preflight().await, Err(LlmError::Unavailable(m)) if m.contains("did not finish")));
}

#[tokio::test]
async fn invalid_config_is_rejected() {
    let err = CliClient::new(CliConfig { effort: "nope".into(), ..CliConfig::default() }).unwrap_err();
    assert!(matches!(err, LlmError::Rejected(_)));
}

#[test]
fn base_args_disable_all_tools_and_set_effort() {
    let args = CliClient::new(CliConfig::default()).unwrap().base_args();
    let at = args.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(args[at + 1], "");
    let mut probe = args.clone();
    set_effort(&mut probe, "low");
    let at = probe.iter().position(|a| a == "--effort").unwrap();
    assert_eq!(probe[at + 1], "low");
}

#[test]
fn estimate_counts_one_token_per_four_chars_rounded_up() {
    assert_eq!(estimate_input_tokens("hello", "abcd", &json!({"a": 1})), 4);
    assert_eq!(estimate_input_tokens("", "x", &json!(null)), 2);
}

#[tokio::test]
#[ignore = "calls the real claude CLI (needs a logged-in subscription)"]
async fn live_claude_returns_schema_constrained_output() {
    let client = CliClient::new(CliConfig { effort: "low".into(), ..CliConfig::default() }).unwrap();
    let schema = json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]});

    client.preflight().await.unwrap();
    let out = client
        .structured::<serde_json::Map<String, Value>>(
            "Be terse.",
            "Return a JSON object with field name set to qgate",
            &schema,
        )
        .await
        .unwrap();

    assert_eq!(out.value["name"], "qgate");
    assert!(out.usage.output_tokens > 0);
}

#[test]
fn sharing_files_swaps_no_tools_for_read_only_in_that_dir() {
    let mut args = CliClient::new(CliConfig::default()).unwrap().base_args();

    enable_read(&mut args, Path::new("/share"));

    let at = args.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(args[at + 1], "Read");
    let at = args.iter().position(|a| a == "--add-dir").unwrap();
    assert_eq!(args[at + 1], "/share");
    let at = args.iter().position(|a| a == "--allowedTools").unwrap();
    assert_eq!(args[at + 1], "Read");
}

#[tokio::test]
async fn structured_with_files_runs_the_cli_in_the_shared_dir_with_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let share = tempfile::tempdir().unwrap();
    let share_path = share.path().canonicalize().unwrap();
    let record = dir.path().join("args.txt");
    let body =
        format!("printf '%s\\n' \"$@\" > {0}; pwd -P >> {0}\n{1}", record.display(), emit(&dir, "ok.json", SUCCESS));
    let client = fake_claude(&dir, &body);

    let out = client.structured_with_files::<Named>("sys", "prompt", &schema(), &share_path).await.unwrap();

    assert_eq!(out.value.name, "qgate");
    let recorded = std::fs::read_to_string(&record).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
    assert!(lines.windows(2).any(|w| w == ["--tools", "Read"]), "{recorded}");
    assert!(lines.windows(2).any(|w| w == ["--add-dir", share_path.to_str().unwrap()]), "{recorded}");
    assert_eq!(lines.last().copied(), share_path.to_str());
}

#[tokio::test]
async fn plain_structured_keeps_every_tool_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("args.txt");
    let body = format!("printf '%s\\n' \"$@\" > {}\n{}", record.display(), emit(&dir, "ok.json", SUCCESS));
    let client = fake_claude(&dir, &body);

    client.structured::<Named>("sys", "prompt", &schema()).await.unwrap();

    let recorded = std::fs::read_to_string(&record).unwrap();
    assert!(!recorded.contains("--add-dir") && !recorded.contains("--allowedTools"), "{recorded}");
}

#[tokio::test]
#[ignore = "calls the real claude CLI (needs a logged-in subscription)"]
async fn live_claude_reads_an_image_through_the_read_tool() {
    let share = tempfile::tempdir().unwrap();
    let share_path = share.path().canonicalize().unwrap();
    let image = share_path.join("frame.png");
    std::fs::write(&image, include_bytes!("fixtures/red_blue.png")).unwrap();
    let client = CliClient::new(CliConfig { effort: "low".into(), ..CliConfig::default() }).unwrap();
    let schema = json!({"type":"object","properties":{"left":{"type":"string"},"right":{"type":"string"}},
        "required":["left","right"],"additionalProperties":false});
    let prompt = format!(
        "Read the image at {} with the Read tool. Give the colour name of its left and right halves.",
        image.display()
    );

    let out = client
        .structured_with_files::<std::collections::HashMap<String, String>>("Be terse.", &prompt, &schema, &share_path)
        .await
        .unwrap();

    assert_eq!(out.value["left"].to_lowercase(), "red");
    assert_eq!(out.value["right"].to_lowercase(), "blue");
}
