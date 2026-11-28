use std::path::Path;
use std::process::Command;

use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::runner::ContextProvenance;
use super::*;
use crate::eval::application::EvalError;
use crate::eval::application::{
    BaseFile, CaseRepository, CaseWorkspace, CurrentProvenance, NewCase, ReviewJob, ReviewRunner,
};
use crate::eval::domain::case::parse_case;
use crate::feedback::application::{FindingFacts, Scope, Verdict};
use crate::history::application::{RecordRun, ReviewRun};
use crate::history::domain::fixtures::{at, live_run};
use crate::llm::{LlmError, Structured, StructuredModel, Usage};
use crate::settings::application::ServerConfig;
use crate::settings::infrastructure::{AppPaths, FileStore};

const QUERY_LINE: &str = "db.Query(\"SELECT * FROM users WHERE id=\" + id)";
const CALL_COST_USD: f64 = 0.25;
const PATCH: &str = "diff --git a/main.go b/main.go\n--- a/main.go\n+++ b/main.go\n@@ -1 +1,5 @@\n package main\n+\n+func lookup(db DB, id string) {\n+\tdb.Query(\"SELECT * FROM users WHERE id=\" + id)\n+}\n";
const CASE_TOML: &str = r#"
id = "sqli"
language = "go"

[ticket]
key = "KAN-1"
title = "Lookup"

[[ac]]
text = "lookup exists"
expect = "met"

[[ac]]
text = "audit log exists"
expect = "not_met"

[[expected]]
file = "main.go"
line = 4
category = "security"
"#;

struct ScriptedModel {
    reports_the_bug: bool,
}

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
            json!({"audits": [{"index": 0, "verdict": "upheld", "counter_file": "", "counter_line": 0,
                "counter_quote": "", "reason": "real"}]})
        } else if self.reports_the_bug {
            findings()
        } else {
            json!({"findings": []})
        };
        let usage = Usage { input_tokens: 10, output_tokens: 5, ..Usage::default() };
        Box::pin(async move { Ok(Structured { value, usage, cost_usd: CALL_COST_USD }) })
    }
}

fn assessments() -> Value {
    json!({"assessments": [
        {"criterion_id": "KAN-1/AC-1", "status": "met", "explanation": "lookup added",
         "evidence": [{"file": "main.go", "line": 4, "quote": QUERY_LINE}]},
        {"criterion_id": "KAN-1/AC-2", "status": "not_met", "explanation": "no audit log", "evidence": []}
    ]})
}

fn findings() -> Value {
    json!({"findings": [
        {"file": "main.go", "line": 4, "severity": "critical", "category": "security", "confidence": "high",
         "title": "SQL injection", "explanation": "id is concatenated into SQL", "evidence": QUERY_LINE,
         "suggestion": "use a placeholder",
         "prevention_rule": "Build SQL only with bound parameters, never by string concatenation.",
         "rule_scope": "**/*.go",
         "reproduce": {"input": "id = \"1 OR 1=1\"", "expected": "only the row with id 1",
                       "actual": "every row of the table", "command": ""}}
    ]})
}

fn context(dir: &Path, reports_the_bug: bool) -> Arc<AppContext> {
    let store = FileStore::new(dir.join("config/credentials.toml"));
    Arc::new(AppContext {
        paths: AppPaths::with_dirs(dir.join("config"), dir.join("data")),
        server: Arc::new(std::sync::RwLock::new(ServerConfig::default())),
        store: Arc::new(store),
        settings_store: Arc::new(
            crate::settings::infrastructure::SqliteServerConfigStore::open(dir.join("data/qgate.db")).unwrap(),
        ),
        env: Arc::new(|_: &str| None),
        model_override: Some(Arc::new(ScriptedModel { reports_the_bug })),
    })
}

fn write_case(cases: &Path) {
    let repo = FsCaseRepository::new(cases);
    let case = NewCase {
        case: parse_case(CASE_TOML).unwrap(),
        base_files: vec![BaseFile { path: "main.go".into(), bytes: b"package main\n".to_vec() }],
        patch: PATCH.into(),
    };
    repo.write(&case).unwrap();
}

fn run_args(cases: &Path) -> EvalRunArgs {
    EvalRunArgs {
        cases: Some(cases.to_path_buf()),
        only: Vec::new(),
        runs: 1,
        with_tests: false,
        baseline: None,
        max_drop: 0.05,
        json: true,
        model: None,
        effort: None,
        config: None,
    }
}

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

#[tokio::test]
async fn the_runner_reviews_the_case_range_with_its_requirements() {
    let work = tempfile::tempdir().unwrap();
    let cases = work.path().join("cases");
    write_case(&cases);
    let loaded = FsCaseRepository::new(&cases).load("sqli").unwrap();
    let repo = GitWorkspace.materialize(&loaded).await.unwrap();
    let job = ReviewJob {
        repo: repo.root.clone(),
        base: repo.base.clone(),
        head: repo.head.clone(),
        requirements: loaded.case.requirements(),
        ticket: None,
        with_tests: false,
    };

    let report = CliReviewRunner::new(context(work.path(), true), Config::default()).run(&job).await.unwrap();

    assert_eq!(report.findings.len(), 1);
    assert_eq!((report.findings[0].file.as_str(), report.findings[0].line), ("main.go", 4));
    let requirements = report.requirements.unwrap();
    assert_eq!(requirements.assessments.len(), 2);
    assert!(report.estimated_cost_usd.unwrap() >= CALL_COST_USD - 1e-9);
}

#[tokio::test]
async fn the_runner_reports_review_failures_as_errors() {
    let work = tempfile::tempdir().unwrap();
    let job = ReviewJob {
        repo: work.path().to_path_buf(),
        base: "deadbeef".into(),
        head: "deadbeef".into(),
        requirements: None,
        ticket: None,
        with_tests: false,
    };

    let error = CliReviewRunner::new(context(work.path(), true), Config::default()).run(&job).await.unwrap_err();

    assert!(matches!(error, EvalError::Runner(_)));
}

#[test]
fn only_a_cases_dir_other_than_the_shipped_ones_is_custom() {
    assert!(!is_custom_cases(None));
    assert!(!is_custom_cases(Some(Path::new(DEFAULT_CASES_DIR))));
    assert!(!is_custom_cases(Some(Path::new(IMAGE_CASES_DIR))));
    assert!(is_custom_cases(Some(Path::new("/tmp/debug-cases"))));
}

#[tokio::test]
async fn an_eval_run_is_scored_stored_and_listed() {
    let work = tempfile::tempdir().unwrap();
    let cases = work.path().join("cases");
    write_case(&cases);
    let context = context(work.path(), true);

    let code = run(context.clone(), run_args(&cases)).await.unwrap();

    assert_eq!(code, ExitCode::from(EXIT_EVAL_OK));
    let stored = open_store(&context).unwrap().latest().unwrap().unwrap();
    assert_eq!(stored.results.len(), 1);
    let score = &stored.results[0];
    assert!(score.expected[0].caught, "the SQL injection finding lands on the expected line");
    assert_eq!(score.criterion_counts().correct, 2);
    assert!(stored.metrics.cost_usd >= CALL_COST_USD - 1e-9);
    assert_eq!(list(&context).unwrap(), ExitCode::from(EXIT_EVAL_OK));
}

#[tokio::test]
async fn a_worse_run_than_the_baseline_exits_with_the_regression_code() {
    let work = tempfile::tempdir().unwrap();
    let cases = work.path().join("cases");
    write_case(&cases);
    let first = run(context(work.path(), true), run_args(&cases)).await.unwrap();
    assert_eq!(first, ExitCode::from(EXIT_EVAL_OK));
    let args = EvalRunArgs { baseline: Some("1".parse().unwrap()), ..run_args(&cases) };

    let same =
        run(context(work.path(), true), EvalRunArgs { baseline: args.baseline, ..run_args(&cases) }).await.unwrap();
    let worse = run(context(work.path(), false), args).await.unwrap();

    assert_eq!(same, ExitCode::from(EXIT_EVAL_OK));
    assert_eq!(worse, ExitCode::from(EXIT_EVAL_REGRESSION));
}

#[tokio::test]
async fn failures_exit_with_the_error_code() {
    let work = tempfile::tempdir().unwrap();
    let cases = work.path().join("cases");
    write_case(&cases);
    let no_baseline =
        EvalArgs { action: None, run: EvalRunArgs { baseline: Some("latest".parse().unwrap()), ..run_args(&cases) } };
    let no_cases = EvalArgs { action: None, run: run_args(&work.path().join("missing")) };

    assert_eq!(eval(context(work.path(), true), no_baseline).await, ExitCode::from(EXIT_EVAL_ERROR));
    assert_eq!(eval(context(work.path(), true), no_cases).await, ExitCode::from(EXIT_EVAL_ERROR));
}

#[tokio::test]
async fn unknown_only_filters_fail_before_any_review() {
    let work = tempfile::tempdir().unwrap();
    let cases = work.path().join("cases");
    write_case(&cases);
    let args = EvalRunArgs { only: vec!["nope".into()], ..run_args(&cases) };

    let error = run(context(work.path(), true), args).await.unwrap_err();

    assert!(error.to_string().contains("nope"));
}

#[tokio::test]
async fn import_run_turns_a_judged_history_run_into_a_case() {
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("main.go"), "package main\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("main.go"), "package main\n\nfunc f() {}\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "change"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    let context = context(work.path(), true);
    let repo_path = repo.display().to_string();
    let recorded = ReviewRun { repo_path: repo_path.clone(), commit: Some(sha), ..live_run("shop", at(1, 1)) };
    let run_id = RecordRun::new(context.open_history().unwrap()).execute(&recorded).unwrap();
    let feedback = FeedbackService::new(Arc::new(
        SqliteFeedbackStore::open(context.paths.data_dir.join(HISTORY_DB_FILE)).unwrap(),
    ));
    let facts = FindingFacts {
        file: "src/<script>.rs".into(),
        category: "security".into(),
        source: "llm".into(),
        evidence: "query(&format!(\"{}\", input))".into(),
    };
    feedback.record(&repo_path, &facts, Verdict::Accepted, Scope::Repo).unwrap();
    let out = work.path().join("out");

    let code = import_run(&context, run_id, &out, None).await.unwrap();

    assert_eq!(code, ExitCode::from(EXIT_EVAL_OK));
    let imported = FsCaseRepository::new(&out).load(&format!("run-{run_id}-qg-7")).unwrap();
    assert_eq!(imported.case.expected.len(), 1);
    assert_eq!(imported.case.language, "go");
    assert!(imported.patch.contains("func f()"));
    assert!(imported.base_dir.join("main.go").is_file());
}

#[tokio::test]
async fn importing_an_unknown_run_exits_with_the_error_code() {
    let work = tempfile::tempdir().unwrap();
    let args = EvalArgs {
        action: Some(EvalAction::ImportRun { run_id: 404, out: work.path().join("out"), id: None }),
        run: run_args(work.path()),
    };

    assert_eq!(eval(context(work.path(), true), args).await, ExitCode::from(EXIT_EVAL_ERROR));
}

#[tokio::test]
async fn current_provenance_uses_the_default_review_model() {
    let work = tempfile::tempdir().unwrap();

    let provenance = ContextProvenance::new(context(work.path(), true)).current().await;

    assert_eq!(provenance.model, Config::default().llm.model.trim());
    assert_eq!(provenance.prompt_hash.len(), 64);
}

#[test]
fn progress_lines_do_not_panic() {
    let score = crate::eval::domain::fixtures::score("a", 0, &[true], 0);

    StderrProgress.scored(&score, 1, 1);
}

#[tokio::test]
async fn with_tests_takes_the_test_agent_path_without_breaking_the_review() {
    let work = tempfile::tempdir().unwrap();
    let cases = work.path().join("cases");
    write_case(&cases);
    let loaded = FsCaseRepository::new(&cases).load("sqli").unwrap();
    let repo = GitWorkspace.materialize(&loaded).await.unwrap();
    let context = context(work.path(), true);
    context.server.write().unwrap().llm.claude_bin = "/definitely/missing/claude".into();
    let job = ReviewJob {
        repo: repo.root.clone(),
        base: repo.base.clone(),
        head: repo.head.clone(),
        requirements: loaded.case.requirements(),
        ticket: None,
        with_tests: true,
    };

    let report = CliReviewRunner::new(context, Config::default()).run(&job).await.unwrap();

    assert_eq!(report.findings.len(), 1, "the normal review still runs");
    assert!(report.estimated_cost_usd.unwrap() >= CALL_COST_USD - 1e-9);
}

#[test]
fn flags_override_the_config_file_which_overrides_the_defaults() {
    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("custom.toml");
    std::fs::write(&file, "[llm]\nmodel = \"claude-sonnet-5-5\"\neffort = \"low\"\n[gate]\nfail_on = \"minor\"\n")
        .unwrap();
    let args = |model: Option<&str>, effort, config: Option<&Path>| EvalRunArgs {
        model: model.map(String::from),
        effort,
        config: config.map(Path::to_path_buf),
        ..run_args(work.path())
    };

    let defaults = resolve_config(&args(None, None, None)).unwrap();
    let from_file = resolve_config(&args(None, None, Some(&file))).unwrap();
    let overridden =
        resolve_config(&args(Some("claude-opus-5-5"), Some(crate::settings::application::Effort::Max), Some(&file)))
            .unwrap();

    assert_eq!(defaults, Config::default());
    assert_eq!(from_file.llm.model, "claude-sonnet-5-5");
    assert_eq!(from_file.llm.effort.as_str(), "low");
    assert_eq!(from_file.gate, overridden.gate);
    assert_ne!(from_file.gate, Config::default().gate);
    assert_eq!(overridden.llm.model, "claude-opus-5-5");
    assert_eq!(overridden.llm.effort.as_str(), "max");
}

#[test]
fn unreadable_or_invalid_config_files_are_errors() {
    let work = tempfile::tempdir().unwrap();
    let invalid = work.path().join("bad.toml");
    std::fs::write(&invalid, "[llm]\nmodel = \"opus\"\n").unwrap();
    let with = |path: std::path::PathBuf| EvalRunArgs { config: Some(path), ..run_args(work.path()) };

    assert!(resolve_config(&with(work.path().join("missing.toml"))).unwrap_err().to_string().contains("cannot read"));
    assert!(resolve_config(&with(invalid)).is_err());
}

#[tokio::test]
async fn the_recorded_provenance_is_the_model_actually_used() {
    let work = tempfile::tempdir().unwrap();
    let cases = work.path().join("cases");
    write_case(&cases);
    let context = context(work.path(), true);
    let args = EvalRunArgs { model: Some("claude-sonnet-5-5".into()), ..run_args(&cases) };

    let code = run(context.clone(), args).await.unwrap();

    assert_eq!(code, ExitCode::from(EXIT_EVAL_OK));
    let stored = open_store(&context).unwrap().latest().unwrap().unwrap();
    assert_eq!(stored.provenance.model, "claude-sonnet-5-5");
}

#[test]
fn cases_default_to_the_working_tree_then_the_image_copy() {
    assert_eq!(default_cases_dir(true), Path::new("eval/cases"));
    assert_eq!(default_cases_dir(false), Path::new("/usr/share/qgate/eval/cases"));
    assert_eq!(resolve_cases_dir(Some("x".into())), Path::new("x"));
}

#[test]
fn model_and_effort_flags_are_validated() {
    use clap::Parser;
    let parse = |extra: &[&str]| {
        let mut argv = vec!["qgate", "eval"];
        argv.extend_from_slice(extra);
        crate::cli::args::Cli::try_parse_from(argv)
    };

    assert!(parse(&["--model", "claude-opus-5-5", "--effort", "xhigh", "--config", "a.toml"]).is_ok());
    assert!(parse(&["--model", "opus"]).is_err());
    assert!(parse(&["--model", "claude-opus-5-5[1m]"]).is_err());
    assert!(parse(&["--effort", "extreme"]).is_err());
}

struct FixedRepos(Vec<crate::watch::domain::repo::WatchedRepo>);

impl crate::watch::application::ports::RepoSource for FixedRepos {
    async fn list(
        &self,
    ) -> Result<Vec<crate::watch::domain::repo::WatchedRepo>, crate::watch::application::ports::WatchError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn registered_repos_report_their_effective_model() {
    use crate::cli::eval::runner::RegisteredRepoModels;
    use crate::eval::application::RepoModels;
    use crate::watch::domain::repo::WatchedRepo;
    let dir = tempfile::tempdir().unwrap();
    let (custom, plain, broken) = (dir.path().join("custom"), dir.path().join("plain"), dir.path().join("broken"));
    for path in [&custom, &plain, &broken] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(custom.join(".qgate.toml"), "[llm]\nmodel = \"claude-opus-5-5\"\n").unwrap();
    std::fs::write(broken.join(".qgate.toml"), "not toml [").unwrap();
    let repos = FixedRepos([custom, plain, broken].into_iter().map(WatchedRepo::from_path).collect());

    let models = RegisteredRepoModels::new(Arc::new(repos)).list().await;

    let names: Vec<(&str, &str)> = models.iter().map(|m| (m.repo.as_str(), m.model.as_str())).collect();
    assert_eq!(
        names,
        [("custom", "claude-opus-5-5"), ("plain", crate::settings::application::Config::default().llm.model.as_str())]
    );
}
