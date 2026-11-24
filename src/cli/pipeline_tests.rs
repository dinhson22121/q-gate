use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;
use serde_json::json;

use super::*;
use crate::history::application::GetRun;
use crate::llm::{LlmError, Structured, Usage};
use crate::review::application::{NoProgress, ToolOutcome};
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

struct ScriptedModel;

const CALL_COST_USD: f64 = 0.25;

impl StructuredModel for ScriptedModel {
    fn structured_json<'a>(
        &'a self,
        _system: &'a str,
        _user_prompt: &'a str,
        schema: &'a Value,
    ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
        let schema_text = schema.to_string();
        let value = if schema_text.contains("criterion_id") {
            assessments()
        } else if schema_text.contains("counter_quote") {
            audits()
        } else {
            findings()
        };
        let usage = Usage { input_tokens: 1000, output_tokens: 200, ..Usage::default() };
        Box::pin(async move { Ok(Structured { value, usage, cost_usd: CALL_COST_USD }) })
    }
}

fn assessments() -> Value {
    json!({"assessments": [
        {"criterion_id": "SHOP-7/AC-1", "status": "met", "explanation": "lookup added",
         "evidence": [{"file": "main.go", "line": 4, "quote": VULNERABLE_LINE.trim()}]},
        {"criterion_id": "SHOP-7/AC-2", "status": "not_met", "explanation": "no audit log", "evidence": []}
    ]})
}

fn audits() -> Value {
    json!({"audits": [
        {"index": 0, "verdict": "upheld", "counter_file": "", "counter_line": 0, "counter_quote": "",
         "reason": "the concatenation is real"}
    ]})
}

fn findings() -> Value {
    json!({"findings": [
        {"file": "main.go", "line": 4, "severity": "critical", "category": "security", "confidence": "high",
         "title": "SQL injection", "explanation": "id is concatenated into SQL",
         "evidence": VULNERABLE_LINE.trim(), "suggestion": "use a placeholder",
         "prevention_rule": "Build SQL only with bound parameters, never by string concatenation.",
         "rule_scope": "**/*.go"},
        {"file": "main.go", "line": 4, "severity": "major", "category": "correctness", "confidence": "high",
         "title": "invented", "explanation": "not real", "evidence": "os.RemoveAll(path)", "suggestion": "",
         "prevention_rule": "Never delete paths taken from input.", "rule_scope": ""}
    ]})
}

fn agents_disabled() -> crate::settings::application::AgentsConfig {
    let mut agents = crate::settings::application::AgentsConfig::default();
    agents.enabled = false;
    agents
}

fn context(dir: &Path) -> AppContext {
    let store = FileStore::new(dir.join("config/credentials.toml"));
    let server = ServerConfig { agents: agents_disabled(), ..ServerConfig::default() };
    AppContext {
        paths: AppPaths::with_dirs(dir.join("config"), dir.join("data")),
        server: Arc::new(std::sync::RwLock::new(server)),
        store: Arc::new(store),
        settings_store: Arc::new(
            crate::settings::infrastructure::SqliteServerConfigStore::open(dir.join("data/qgate.db")).unwrap(),
        ),
        env: Arc::new(|_: &str| None),
        model_override: Some(Arc::new(ScriptedModel)),
    }
}

#[tokio::test]
async fn commit_review_runs_checks_verifies_findings_and_records_history() {
    let repo = tempfile::tempdir().unwrap();
    let config =
        "[[checks]]\nname = \"has-main\"\ncommand = [\"git\", \"--version\"]\nrequired = true\ntimeout_secs = 30\n";
    let sha = repo_with_commit(repo.path(), config);
    let work = tempfile::tempdir().unwrap();
    let context = Arc::new(context(work.path()));
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
    assert!(report.audited, "the audit pass ran and upheld the finding");
    assert_eq!(report.rejected.len(), 1);
    assert!(report.requirements.is_none(), "no ticket source is configured");
    assert_eq!(report.checks.len(), 1);
    assert!(!report.checks[0].is_blocking(), "check ran in the snapshot: {:?}", report.checks[0]);
    assert!(!report.gate.passed);
    assert_eq!(report.gate.blocking, vec![0]);
    assert!(report.gate.unmet_criteria.is_empty());
    let cost = report.estimated_cost_usd.expect("CLI cost is summed into the report");
    assert!(cost >= CALL_COST_USD - 1e-9, "review cost is reported: {cost}");

    let run_id = outcome.run_id.expect("run recorded");
    let stored = GetRun::new(history).execute(run_id).unwrap().expect("run exists");
    assert_eq!(stored.commit.as_deref(), Some(sha.as_str()));
    assert_eq!(stored.ticket_key, None);
    assert_eq!(stored.status, RunStatus::Failed);
    let leftovers = std::fs::read_dir(work.path().join("data/work")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftovers, 0, "snapshot cleaned up");

    let rule = report.findings[0].prevention_rule.as_ref().expect("the model's prevention rule is kept");
    assert_eq!(rule.scope.as_deref(), Some("**/*.go"));
    let rules = learned_rules(repo.path());
    assert!(rules.contains("Build SQL only with bound parameters"), "{rules}");
    assert!(!rules.contains("Never delete paths"), "rules of rejected findings are not learned");
}

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
    let context = Arc::new(context(work.path()));
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

#[tokio::test]
async fn quality_and_regression_flags_from_the_server_settings_skip_their_checks() {
    let repo = tempfile::tempdir().unwrap();
    let head = repo_with_commit(repo.path(), "");
    let data = tempfile::tempdir().unwrap();
    let run = |quality: bool, regression: bool| {
        let (repo, data, head) = (repo.path().to_path_buf(), data.path().to_path_buf(), head.clone());
        async move {
            let mut ctx = context(&data);
            {
                let mut server = ctx.server.write().unwrap();
                server.quality.enabled = quality;
                server.regression.enabled = regression;
            }
            ctx.env = Arc::new(|_: &str| None);
            let pipeline = ReviewPipeline::new(Arc::new(ctx), None);
            let request = ReviewRequest {
                repo_root: repo.clone(),
                target: DiffTarget::Commit(head.clone()),
                trigger: Trigger::Commit,
                explicit_ticket: None,
                ticket_in_progress: false,
            };
            let server = pipeline.context.server();
            let cache = ReviewCache::open(&data.join("qgate.db"), true);
            let checks = pipeline
                .supplementary_checks(
                    &request,
                    &server,
                    &data.join("qgate.db"),
                    &data.join("work"),
                    &pipeline.context.exec_backend(),
                    &cache,
                )
                .unwrap();
            checks.run(&repo).await
        }
    };
    let both_off = run(false, false).await;
    assert_eq!(both_off, ToolOutcome::default(), "disabled checks leave no findings and no notes");
    let regression_only = run(false, true).await;
    assert!(regression_only.notes.iter().all(|n| n.contains("regression")), "{:?}", regression_only.notes);
    assert!(!regression_only.notes.is_empty(), "an enabled check reports why it could not run");
}

#[test]
fn structured_model_uses_the_override_else_validates_the_cli_settings() {
    let work = tempfile::tempdir().unwrap();
    let mut ctx = context(work.path());
    assert!(ctx.structured_model("claude-opus-5-5", "high").is_ok(), "the override is returned as is");

    ctx.model_override = None;
    assert!(ctx.structured_model("claude-opus-5-5", "high").is_ok());
    let rejected = |model: &str, effort: &str| match ctx.structured_model(model, effort) {
        Err(LlmError::Rejected(reason)) => reason,
        other => panic!("expected Rejected, got {:?}", other.map(|_| ())),
    };
    assert!(rejected("claude-opus-5-5", "huge").contains("effort"));
    assert!(rejected("--dangerous", "high").contains("model"));
    ctx.server.write().unwrap().llm.claude_bin = "/opt/claude".into();
    ctx.server.write().unwrap().llm.timeout_secs = 42;
    let config = ctx.cli_config("claude-opus-5-5", "high");
    assert_eq!(config.claude_bin, std::path::PathBuf::from("/opt/claude"));
    assert_eq!(config.timeout, std::time::Duration::from_secs(42));
}

struct LimitedModel {
    calls: std::sync::atomic::AtomicUsize,
}

impl StructuredModel for LimitedModel {
    fn structured_json<'a>(
        &'a self,
        _system: &'a str,
        _user_prompt: &'a str,
        _schema: &'a Value,
    ) -> BoxFuture<'a, Result<Structured<Value>, LlmError>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(LlmError::Unavailable("must not be called".into())) })
    }

    fn preflight(&self) -> BoxFuture<'_, Result<(), LlmError>> {
        Box::pin(async {
            Err(LlmError::RateLimited { resets_at: Some("4:30pm".into()), message: "You've hit your limit".into() })
        })
    }
}

#[tokio::test]
async fn a_rate_limited_preflight_aborts_the_review_before_any_agent_and_records_an_error() {
    let repo = tempfile::tempdir().unwrap();
    let sha = repo_with_commit(repo.path(), "");
    let work = tempfile::tempdir().unwrap();
    let limited = Arc::new(LimitedModel { calls: Default::default() });
    let mut context = context(work.path());
    context.model_override = Some(limited.clone());
    let context = Arc::new(context);
    let history = context.open_history().unwrap();
    let pipeline = ReviewPipeline::new(context, Some(history.clone()));
    let request = ReviewRequest {
        repo_root: repo.path().to_path_buf(),
        target: DiffTarget::Commit(sha),
        trigger: Trigger::Push,
        explicit_ticket: None,
        ticket_in_progress: false,
    };

    let error = pipeline.run(&request, &NoProgress).await.err().expect("a rate-limited review must not pass");

    assert!(format!("{error:#}").contains("waiting for session reset (resets 4:30pm)"), "{error:#}");
    assert_eq!(limited.calls.load(std::sync::atomic::Ordering::SeqCst), 0, "no agent ran");
    let runs = history.list(&crate::history::application::RunFilter { limit: 10, ..Default::default() }).unwrap();
    assert_eq!((runs.len(), runs[0].status), (1, RunStatus::Error), "recorded as an error, never as a pass");
    assert!(runs[0].error.as_deref().unwrap().contains("not running"));
}
