use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::llm::LlmError;
use crate::review::application::{AcceptanceCriterion, Requirements, TestVerdict};
use crate::review::domain::change_set::FileChange;
use crate::review::domain::change_set::fixtures::{added, file};
use crate::sandbox::infrastructure::LocalBackend;
use crate::testing::application::fakes::{
    CopySnapshots, FakeRunner, GO_ASSERTION_OUTPUT, GO_BUILD_OUTPUT, ScriptedLoop, failed_run,
};
use crate::testing::domain::classify::RawRun;
use crate::testing::infrastructure::{ExecTestRunner, SnapshotWorkspace};

const CALC: &str = "package calc\n\n// Add returns the sum of a and b.\nfunc Add(a, b int) int {\n\treturn a - b\n}\n";
const FAILING_TEST: &str = "package calc\n\nimport \"testing\"\n\nfunc TestQgateS1(t *testing.T) {\n\tif got := Add(2, 3); got != 5 {\n\t\tt.Errorf(\"Add(2, 3) = %d, want 5\", got)\n\t}\n}\n";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn go_module() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("go.mod"), "module example.com/m\n\ngo 1.21\n").unwrap();
    fs::create_dir(dir.path().join("calc")).unwrap();
    fs::write(dir.path().join("calc/calc.go"), CALC).unwrap();
    dir
}

fn changes() -> Vec<FileChange> {
    vec![file("calc/calc.go", 5, vec![added(5, "\treturn a - b")], Some(CALC)), file("README.md", 1, vec![], None)]
}

fn requirements() -> Requirements {
    Requirements {
        key: "SHOP-7".into(),
        title: "calculator".into(),
        url: None,
        criteria: vec![AcceptanceCriterion { id: "AC-1".into(), text: "Add returns the sum of both numbers".into() }],
    }
}

fn scenario() -> TestScenario {
    TestScenario {
        id: "S1".into(),
        criterion_ids: vec!["SHOP-7/AC-1".into()],
        title: "sum".into(),
        given: "two numbers 2 and 3".into(),
        when: "Add is called".into(),
        then: "it returns 5".into(),
    }
}

fn settings(languages: Vec<TestLanguage>) -> TestAgentSettings {
    TestAgentSettings { languages, max_repair_rounds: 2, timeout: Duration::from_secs(120), max_turns: 20 }
}

fn input<'a>(files: &'a [FileChange], requirements: &'a Requirements, snapshot: Option<&'a Path>) -> AgentInput<'a> {
    AgentInput {
        files,
        requirements: Some(requirements),
        ticket: None,
        commit: COMMIT,
        repo_root: Path::new("/repos/shop"),
        snapshot_root: snapshot,
    }
}

fn happy_script(verdict: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("list_files", json!({ "dir": "calc" })),
        ("read_file", json!({ "path": "calc/calc.go" })),
        ("write_test", json!({ "path": "calc/qgate_s1_test.go", "content": FAILING_TEST })),
        ("run_test", json!({})),
        (
            "report_result",
            json!({
                "verdict": verdict,
                "code_under_test": [{ "file": "calc/calc.go", "line": 5, "quote": "return a - b" }],
                "explanation": "Add subtracts",
            }),
        ),
    ]
}

type FakeAgent = ScenarioTestAgent<ScriptedLoop, FakeRunner, SnapshotWorkspace>;

fn snapshots(snapshot: Option<&Path>) -> Arc<CopySnapshots> {
    Arc::new(CopySnapshots::of(snapshot.unwrap_or(Path::new("/definitely/not/a/snapshot"))))
}

async fn run_fake(script: ScriptedLoop, runner: FakeRunner, snapshot: Option<&Path>) -> TestResult {
    let agent =
        FakeAgent::with_ports(Arc::new(script), runner, snapshots(snapshot), settings(TestLanguage::ALL.to_vec()));
    let (files, requirements) = (changes(), requirements());
    agent.run_scenario(&input(&files, &requirements, snapshot), &scenario()).await
}

fn assert_complete_failure(result: &TestResult) {
    assert_eq!(result.verdict, TestVerdict::FailedAssertion, "{}", result.note);
    let evidence = result.evidence.as_ref().unwrap();
    assert!(evidence.missing_parts().is_empty(), "{:?}", evidence.missing_parts());
    assert_eq!(evidence.criterion_ids, ["SHOP-7/AC-1"]);
    assert_eq!(evidence.criterion_texts, ["Add returns the sum of both numbers"]);
    assert_eq!(evidence.scenario, scenario());
    assert_eq!(evidence.test_file, "calc/qgate_s1_test.go");
    assert_eq!(evidence.test_source, FAILING_TEST);
    assert_eq!(evidence.command, ["go", "test", "./calc", "-run", "^TestQgateS1", "-count=1"]);
    assert_eq!(evidence.exit_code, Some(1));
    assert!(evidence.output.contains("want 5"), "{}", evidence.output);
    assert_eq!(evidence.code_under_test[0].file, "calc/calc.go");
    assert_eq!(evidence.commit, COMMIT);
    assert!(evidence.reproduce.starts_with("cd /repos/shop && go test ./calc -run '^TestQgateS1' -count=1"));
}

#[tokio::test]
async fn end_to_end_failure_carries_complete_evidence() {
    let dir = go_module();
    let script = ScriptedLoop::new(happy_script("failed_assertion"));
    let runner = FakeRunner::with(vec![failed_run(GO_BUILD_OUTPUT), failed_run(GO_ASSERTION_OUTPUT)]);
    // First run fails to build; the agent rewrites and runs again (one repair round).
    let mut steps = script.steps.clone();
    steps.insert(4, ("write_test".into(), json!({ "path": "calc/qgate_s1_test.go", "content": FAILING_TEST })));
    steps.insert(5, ("run_test".into(), json!({})));
    let result = run_fake(ScriptedLoop { steps, ..ScriptedLoop::default() }, runner, Some(dir.path())).await;
    assert_complete_failure(&result);
    assert_eq!(result.language, "go");
}

#[tokio::test]
async fn model_cannot_turn_a_passing_run_into_a_failure() {
    let dir = go_module();
    let runner = FakeRunner::with(vec![RawRun { exit_code: Some(0), output: "ok".into(), ..RawRun::default() }]);
    let result = run_fake(ScriptedLoop::new(happy_script("failed_assertion")), runner, Some(dir.path())).await;
    assert_eq!(result.verdict, TestVerdict::Passed);
    assert!(result.note.contains("advisory only"));
}

#[tokio::test]
async fn model_cannot_hide_a_failing_run() {
    let dir = go_module();
    let runner = FakeRunner::with(vec![failed_run(GO_ASSERTION_OUTPUT)]);
    let result = run_fake(ScriptedLoop::new(happy_script("passed")), runner, Some(dir.path())).await;
    assert_eq!(result.verdict, TestVerdict::FailedAssertion);
}

#[tokio::test]
async fn failure_without_code_reference_is_inconclusive() {
    let dir = go_module();
    let mut steps = happy_script("failed_assertion");
    steps.truncate(4);
    let runner = FakeRunner::with(vec![failed_run(GO_ASSERTION_OUTPUT)]);
    let result = run_fake(ScriptedLoop::new(steps), runner, Some(dir.path())).await;
    assert_eq!(result.verdict, TestVerdict::Inconclusive);
    assert!(result.note.contains("code under test"), "{}", result.note);
    assert!(result.note.contains("without report_result"), "{}", result.note);
}

#[tokio::test]
async fn never_touches_the_repository_without_a_snapshot() {
    let script = ScriptedLoop::new(happy_script("failed_assertion"));
    let result = run_fake(script, FakeRunner::default(), None).await;
    assert_eq!(result.verdict, TestVerdict::Inconclusive);
    assert!(result.note.contains("no writable snapshot"));
}

#[tokio::test]
async fn unsupported_setups_are_inconclusive() {
    let dir = go_module();
    let (files, requirements) = (changes(), requirements());
    let agent = FakeAgent::with_ports(
        Arc::new(ScriptedLoop::default()),
        FakeRunner::default(),
        snapshots(Some(dir.path())),
        settings(vec![TestLanguage::Java]),
    );
    let result = agent.run_scenario(&input(&files, &requirements, Some(dir.path())), &scenario()).await;
    assert_eq!(result.note, "language not enabled: go");

    fs::remove_file(dir.path().join("go.mod")).unwrap();
    let result = run_fake(ScriptedLoop::default(), FakeRunner::default(), Some(dir.path())).await;
    assert!(result.note.contains("no go module"), "{}", result.note);

    let missing = Path::new("/definitely/not/a/snapshot");
    let result = run_fake(ScriptedLoop::default(), FakeRunner::default(), Some(missing)).await;
    assert!(result.note.contains("cannot create the scenario's snapshot"), "{}", result.note);
}

#[tokio::test]
async fn loop_errors_and_toolchain_gaps_are_reported() {
    let dir = go_module();
    let script = ScriptedLoop { fail_with: Some(LlmError::Unavailable("down".into())), ..ScriptedLoop::default() };
    let result = run_fake(script, FakeRunner::default(), Some(dir.path())).await;
    assert_eq!(result.verdict, TestVerdict::Inconclusive);
    assert!(result.note.contains("agent loop failed"), "{}", result.note);

    let missing = RawRun { spawn_error: Some("no go".into()), not_found: true, ..RawRun::default() };
    let result =
        run_fake(ScriptedLoop::new(happy_script("inconclusive")), FakeRunner::with(vec![missing]), Some(dir.path()))
            .await;
    assert!(result.note.contains("install go via QGATE_TOOLCHAINS"), "{}", result.note);
}

#[tokio::test]
async fn prompt_names_the_allowed_path_and_fences_the_scenario() {
    let dir = go_module();
    let script = Arc::new(ScriptedLoop::default());
    let agent = FakeAgent::with_ports(
        script.clone(),
        FakeRunner::default(),
        snapshots(Some(dir.path())),
        settings(TestLanguage::ALL.to_vec()),
    );
    let (files, requirements) = (changes(), requirements());
    let dyn_agent: &dyn TestAgent = &agent;
    dyn_agent.run(&input(&files, &requirements, Some(dir.path())), &scenario()).await;
    let prompt = script.prompts.lock().unwrap()[0].clone();
    assert!(prompt.contains("calc/qgate_s1_test.go"));
    assert!(prompt.contains("<untrusted_scenario>") && prompt.contains("it returns 5"));
}

/// Real `go test` run: the handler writes a failing test against a buggy `Add`.
/// Skips (passes vacuously) when `go` is not installed.
#[tokio::test]
async fn real_go_run_yields_failed_assertion_with_complete_evidence() {
    if std::process::Command::new("go").arg("version").output().is_err() {
        eprintln!("skipped: go is not installed");
        return;
    }
    let dir = go_module();
    let script = ScriptedLoop::new(happy_script("failed_assertion"));
    let agent = ScenarioTestAgent::<_, _, SnapshotWorkspace>::with_ports(
        Arc::new(script),
        ExecTestRunner::new(Arc::new(LocalBackend)),
        snapshots(Some(dir.path())),
        settings(TestLanguage::ALL.to_vec()),
    );
    let (files, requirements) = (changes(), requirements());
    let result = agent.run_scenario(&input(&files, &requirements, Some(dir.path())), &scenario()).await;
    assert_complete_failure(&result);
    assert!(result.evidence.unwrap().output.contains("qgate_s1_test.go:7: Add(2, 3) = -1, want 5"));
}

#[tokio::test]
async fn each_scenario_works_in_its_own_snapshot() {
    let dir = go_module();
    let snapshots = snapshots(Some(dir.path()));
    let (files, requirements) = (changes(), requirements());
    for _ in 0..2 {
        let runner = FakeRunner::with(vec![failed_run(GO_ASSERTION_OUTPUT)]);
        let script = ScriptedLoop::new(happy_script("failed_assertion"));
        let agent =
            FakeAgent::with_ports(Arc::new(script), runner, snapshots.clone(), settings(TestLanguage::ALL.to_vec()));
        let result = agent.run_scenario(&input(&files, &requirements, Some(dir.path())), &scenario()).await;
        assert_eq!(result.verdict, TestVerdict::FailedAssertion, "{}", result.note);
    }
    let created = snapshots.created.lock().unwrap().clone();
    assert_eq!(created.len(), 2);
    assert_ne!(created[0], created[1]);
    assert!(created.iter().all(|path| !path.exists()), "scenario snapshots are deleted afterwards");
    assert!(!dir.path().join("calc/qgate_s1_test.go").exists(), "the shared snapshot is never written");
}

#[tokio::test]
async fn unreachable_runner_makes_the_scenario_inconclusive_with_the_reason() {
    let dir = go_module();
    let down = RawRun {
        spawn_error: Some("qgate runner unavailable at /work/.runner/runner.sock".into()),
        ..RawRun::default()
    };
    let runner = FakeRunner::with(vec![down]);
    let result = run_fake(ScriptedLoop::new(happy_script("failed_assertion")), runner, Some(dir.path())).await;
    assert_eq!(result.verdict, TestVerdict::Inconclusive);
    assert!(result.note.contains("could not be started: qgate runner unavailable"), "{}", result.note);
}
