//! End-to-end pipeline test: real git repo, real checks, real SQLite history; Claude and Jira mocked over HTTP.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::history::application::GetRun;
use crate::review::application::{CriterionStatus, NoProgress};
use crate::settings::application::{JiraConfig, ServerLlmConfig, store_secret};
use crate::settings::infrastructure::FileStore;

const VULNERABLE_LINE: &str = "\tdb.Query(\"SELECT * FROM users WHERE id=\" + id)";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `config` is committed in the first commit, so it governs the reviewed (second) commit.
fn repo_with_commit(dir: &Path, config: &str) -> String {
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("main.go"), "package main\n").unwrap();
    std::fs::write(dir.join(".qgate.toml"), config).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
    let code = format!("package main\n\nfunc lookup(db DB, id string) {{\n{VULNERABLE_LINE}\n}}\n");
    std::fs::write(dir.join("main.go"), code).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "SHOP-7: add user lookup"]);
    git(dir, &["rev-parse", "HEAD"])
}

fn claude_reply(answer: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": "claude-opus-5-5",
        "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": answer.to_string()}],
        "stop_reason": "end_turn",
        "stop_details": null,
        "usage": {"input_tokens": 1000, "output_tokens": 200, "cache_read_input_tokens": 0}
    }))
}

async fn mock_services() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/SHOP-7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key": "SHOP-7",
            "fields": {
                "summary": "User lookup",
                "status": {"name": "In Progress"},
                "description": "Acceptance Criteria:\n- Users can be looked up by id\n- Lookup is audited"
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("criterion_id"))
        .respond_with(claude_reply(json!({"assessments": [
            {"criterion_id": "SHOP-7/AC-1", "status": "met", "explanation": "lookup added",
             "evidence": [{"file": "main.go", "line": 4, "quote": VULNERABLE_LINE.trim()}]},
            {"criterion_id": "SHOP-7/AC-2", "status": "not_met", "explanation": "no audit log", "evidence": []}
        ]})))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(claude_reply(json!({"findings": [
            {"file": "main.go", "line": 4, "severity": "critical", "category": "security", "confidence": "high",
             "title": "SQL injection", "explanation": "id is concatenated into SQL",
             "evidence": VULNERABLE_LINE.trim(), "suggestion": "use a placeholder",
             "prevention_rule": "Build SQL only with bound parameters, never by string concatenation.",
             "rule_scope": "**/*.go"},
            {"file": "main.go", "line": 4, "severity": "major", "category": "correctness", "confidence": "high",
             "title": "invented", "explanation": "not real", "evidence": "os.RemoveAll(path)", "suggestion": "",
             "prevention_rule": "Never delete paths taken from input.", "rule_scope": ""}
        ]})))
        .with_priority(5)
        .mount(&server)
        .await;
    server
}

fn context(dir: &Path, mock_uri: &str) -> AppContext {
    let store = FileStore::new(dir.join("config/credentials.toml"));
    store_secret(CredentialName::AnthropicApiKey, "sk-ant-test", &store).unwrap();
    store_secret(CredentialName::JiraApiToken, "jira-token", &store).unwrap();
    let server = ServerConfig {
        llm: ServerLlmConfig { base_url: mock_uri.to_string() },
        // Ticket agents have their own tests; this one covers the core pipeline.
        agents: crate::settings::application::AgentsConfig { enabled: false, ..Default::default() },
        jira: JiraConfig {
            enabled: true,
            base_url: mock_uri.to_string(),
            email: "dev@example.com".into(),
            ..JiraConfig::default()
        },
        ..ServerConfig::default()
    };
    AppContext {
        paths: AppPaths::with_dirs(dir.join("config"), dir.join("data")),
        server: Arc::new(std::sync::RwLock::new(server)),
        store: Arc::new(store),
        settings_store: Arc::new(
            crate::settings::infrastructure::SqliteServerConfigStore::open(dir.join("data/qgate.db")).unwrap(),
        ),
        env: Arc::new(|_: &str| None),
    }
}

#[tokio::test]
async fn commit_review_runs_checks_assesses_ticket_verifies_findings_and_records_history() {
    let mock = mock_services().await;
    let repo = tempfile::tempdir().unwrap();
    let config =
        "[[checks]]\nname = \"has-main\"\ncommand = [\"git\", \"--version\"]\nrequired = true\ntimeout_secs = 30\n";
    let sha = repo_with_commit(repo.path(), config);
    let work = tempfile::tempdir().unwrap();
    let context = Arc::new(context(work.path(), &mock.uri()));
    let history = context.open_history().unwrap();
    let pipeline = ReviewPipeline::new(context.clone(), Some(history.clone()));

    let request = ReviewRequest {
        repo_root: repo.path().to_path_buf(),
        target: DiffTarget::Commit(sha.clone()),
        trigger: Trigger::Commit,
        explicit_ticket: None,
        ticket_in_progress: false,
    };
    let outcome = pipeline.run(&request, &NoProgress).await.unwrap();
    let report = &outcome.report;

    assert_eq!(report.findings.len(), 1, "hallucinated finding is dropped");
    assert_eq!(report.findings[0].title, "SQL injection");
    assert_eq!(report.rejected.len(), 1);
    let requirements = report.requirements.as_ref().expect("ticket detected from commit message");
    assert_eq!(requirements.requirements.key, "SHOP-7");
    let statuses: Vec<_> = requirements.assessments.iter().map(|a| a.status).collect();
    assert_eq!(statuses, vec![CriterionStatus::Met, CriterionStatus::NotMet]);
    assert_eq!(report.checks.len(), 1);
    assert!(!report.checks[0].is_blocking(), "check ran in the snapshot: {:?}", report.checks[0]);
    assert!(!report.gate.passed);
    assert_eq!(report.gate.blocking, vec![0]);
    assert_eq!(report.gate.unmet_criteria, vec!["SHOP-7/AC-2"]);
    assert!(report.estimated_cost_usd.is_some());

    let run_id = outcome.run_id.expect("run recorded");
    let stored = GetRun::new(history).execute(run_id).unwrap().expect("run exists");
    assert_eq!(stored.commit.as_deref(), Some(sha.as_str()));
    assert_eq!(stored.ticket_key.as_deref(), Some("SHOP-7"));
    assert_eq!(stored.status, RunStatus::Failed);
    let leftovers = std::fs::read_dir(work.path().join("data/work")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftovers, 0, "snapshot cleaned up");

    let rule = report.findings[0].prevention_rule.as_ref().expect("the model's prevention rule is kept");
    assert_eq!(rule.scope.as_deref(), Some("**/*.go"));
    let rules = learned_rules(repo.path());
    assert!(rules.contains("Build SQL only with bound parameters"), "{rules}");
    assert!(!rules.contains("Never delete paths"), "rules of rejected findings are not learned");
}

/// The per-repo rules file the export wrote (outside Docker: below `<repo>/.qgate/reviews`).
fn learned_rules(repo: &Path) -> String {
    let reviews = crate::handoff::native_default_dir(repo);
    let repo_dir = std::fs::read_dir(&reviews).unwrap().next().expect("a folder per repository").unwrap().path();
    std::fs::read_to_string(repo_dir.join("qgate-rules.md")).expect("rules file written")
}

#[tokio::test]
async fn review_error_is_recorded_and_returned() {
    let repo = tempfile::tempdir().unwrap();
    let sha = repo_with_commit(repo.path(), "[review]\nignore = [\"a[\"]\n");
    let work = tempfile::tempdir().unwrap();
    let context = Arc::new(context(work.path(), "https://example.atlassian.net"));
    let history = context.open_history().unwrap();
    let pipeline = ReviewPipeline::new(context, Some(history.clone()));
    let request = ReviewRequest {
        repo_root: repo.path().to_path_buf(),
        target: DiffTarget::Commit(sha),
        trigger: Trigger::Commit,
        explicit_ticket: Some("not a key".into()),
        ticket_in_progress: false,
    };
    let error = pipeline.run(&request, &NoProgress).await.err().expect("invalid ignore pattern fails");
    assert!(format!("{error:#}").contains("a["));
    let runs = history.list(&crate::history::application::RunFilter { limit: 10, ..Default::default() }).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Error);
}

#[tokio::test]
async fn a_commit_cannot_weaken_the_config_that_reviews_it() {
    let repo = tempfile::tempdir().unwrap();
    repo_with_commit(repo.path(), "[gate]\nfail_on = \"major\"\n");
    std::fs::write(repo.path().join(".qgate.toml"), "[gate]\nfail_on = \"critical\"\n").unwrap();
    git(repo.path(), &["commit", "-q", "-am", "relax the gate"]);
    let head = git(repo.path(), &["rev-parse", "HEAD"]);

    let config = load_review_config(repo.path(), &DiffTarget::Commit(head)).await.unwrap();
    assert_eq!(config.gate.fail_on, crate::review::application::Severity::Major, "parent's config applies");
    let working = load_review_config(repo.path(), &DiffTarget::WorkingTree).await.unwrap();
    assert_eq!(working.gate.fail_on, crate::review::application::Severity::Critical);
}

#[tokio::test]
async fn root_commit_is_reviewed_with_default_config() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join(".qgate.toml"), "[gate]\nfail_on = \"info\"\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "root"]);
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let config = load_review_config(repo.path(), &DiffTarget::Commit(head)).await.unwrap();
    assert_eq!(config, Config::default());
}
