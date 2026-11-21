use std::fs;

use serde_json::json;

use super::*;
use crate::testing::application::fakes::{FakeRunner, GO_ASSERTION_OUTPUT, GO_BUILD_OUTPUT, failed_run};
use crate::testing::domain::classify::RawRun;
use crate::testing::infrastructure::SnapshotWorkspace;

const SOURCE: &str = "package calc\n\nfunc Add(a, b int) int {\n\treturn a - b\n}\n";
const TEST: &str = "package calc\n\nimport \"testing\"\n\nfunc TestQgateS1(t *testing.T) {}\n";

struct Fixture {
    dir: tempfile::TempDir,
    workspace: SnapshotWorkspace,
    targets: Vec<TestTarget>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("go.mod"), "module example.com/m\n").unwrap();
    fs::create_dir(dir.path().join("calc")).unwrap();
    fs::write(dir.path().join("calc/calc.go"), SOURCE).unwrap();
    fs::write(dir.path().join("calc/existing_test.go"), "package calc\n").unwrap();
    let workspace = SnapshotWorkspace::open(dir.path()).unwrap();
    let targets = vec![TestTarget {
        language: TestLanguage::Go,
        module_dir: String::new(),
        test_file: "calc/qgate_s1_test.go".into(),
        command: ["go", "test", "./calc", "-run", "^TestQgateS1", "-count=1"].map(String::from).to_vec(),
    }];
    Fixture { dir, workspace, targets }
}

fn limits(max_repair_rounds: u32) -> HandlerLimits {
    HandlerLimits { language: TestLanguage::Go, timeout: Duration::from_secs(5), max_repair_rounds }
}

fn write(path: &str) -> Value {
    json!({ "path": path, "content": TEST })
}

fn report(verdict: &str, refs: Value) -> Value {
    json!({ "verdict": verdict, "code_under_test": refs, "explanation": "because" })
}

#[tokio::test]
async fn read_tools_work_inside_the_snapshot() {
    let f = fixture();
    let runner = FakeRunner::default();
    let h = ScenarioToolHandler::new(&f.workspace, &runner, &f.targets, limits(1));

    let listing = h.call(LIST_FILES, json!({ "dir": "." })).await;
    assert_eq!(listing, ToolOutput::ok("calc/\ngo.mod"));
    let read = h.call(READ_FILE, json!({ "path": "calc/calc.go", "start_line": 3, "end_line": 4 })).await;
    assert!(!read.is_error);
    assert_eq!(read.content, "calc/calc.go (lines 3-4 of 5)\n3\tfunc Add(a, b int) int {\n4\t\treturn a - b\n");
    let found = h.call(SEARCH, json!({ "pattern": "return a", "glob": "**/*.go" })).await;
    assert_eq!(found.content, "calc/calc.go:4: return a - b");
    assert_eq!(h.call(SEARCH, json!({ "pattern": "zzz" })).await.content, "no matches");
    assert!(h.call(SEARCH, json!({ "pattern": "" })).await.is_error);
    assert!(h.call(READ_FILE, json!({ "path": "../etc/passwd" })).await.is_error);
    assert!(h.call(LIST_FILES, json!({ "dir": "/" })).await.is_error);
    assert!(h.call(READ_FILE, json!({ "path": "x", "extra": 1 })).await.content.contains("invalid input"));
    assert!(h.call("bash", json!({})).await.content.contains("unknown tool"));
    assert!(!h.finished());
}

#[tokio::test]
async fn read_file_caps_lines() {
    let f = fixture();
    let big: String = (1..=2500).map(|i| format!("line {i}\n")).collect();
    fs::write(f.dir.path().join("big.txt"), big).unwrap();
    let runner = FakeRunner::default();
    let h = ScenarioToolHandler::new(&f.workspace, &runner, &f.targets, limits(1));
    let out = h.call(READ_FILE, json!({ "path": "big.txt" })).await.content;
    assert!(out.starts_with("big.txt (lines 1-2000 of 2500)"));
    assert!(out.ends_with("2000\tline 2000\n"));
}

#[tokio::test]
async fn write_test_only_accepts_the_allowed_new_path() {
    let f = fixture();
    let runner = FakeRunner::default();
    let h = ScenarioToolHandler::new(&f.workspace, &runner, &f.targets, limits(1));

    for bad in ["calc/calc.go", "calc/other_test.go", "../qgate_s1_test.go", "/tmp/x_test.go"] {
        let out = h.call(WRITE_TEST, write(bad)).await;
        assert!(out.is_error, "{bad} must be rejected");
    }
    assert_eq!(fs::read_to_string(f.dir.path().join("calc/calc.go")).unwrap(), SOURCE, "production code untouched");
    assert!(h.call(WRITE_TEST, json!({ "path": "calc/qgate_s1_test.go", "content": " " })).await.is_error);

    let ok = h.call(WRITE_TEST, write("./calc/qgate_s1_test.go")).await;
    assert!(!ok.is_error, "{}", ok.content);
    assert_eq!(fs::read_to_string(f.dir.path().join("calc/qgate_s1_test.go")).unwrap(), TEST);
    assert!(!h.call(WRITE_TEST, json!({ "path": "calc/qgate_s1_test.go", "content": "v2" })).await.is_error);
    let second = h.call(WRITE_TEST, write("calc/existing_test.go")).await;
    assert!(second.content.contains("only one test file"));
}

#[tokio::test]
async fn write_test_refuses_an_allowed_path_that_already_exists() {
    let f = fixture();
    fs::write(f.dir.path().join("calc/qgate_s1_test.go"), "planted").unwrap();
    let runner = FakeRunner::default();
    let h = ScenarioToolHandler::new(&f.workspace, &runner, &f.targets, limits(1));
    assert!(h.call(WRITE_TEST, write("calc/qgate_s1_test.go")).await.content.contains("already exists"));
    assert_eq!(fs::read_to_string(f.dir.path().join("calc/qgate_s1_test.go")).unwrap(), "planted");
}

#[tokio::test]
async fn run_test_uses_the_fixed_command_and_counts_repairs() {
    let f = fixture();
    let runner = FakeRunner::with(vec![failed_run(GO_BUILD_OUTPUT), failed_run(GO_BUILD_OUTPUT)]);
    let h = ScenarioToolHandler::new(&f.workspace, &runner, &f.targets, limits(1));

    assert!(h.call(RUN_TEST, json!({})).await.content.contains("write_test first"));
    h.call(WRITE_TEST, write("calc/qgate_s1_test.go")).await;
    assert!(h.call(RUN_TEST, json!({ "command": "rm -rf /" })).await.is_error, "no free-form commands");
    let first: Value = serde_json::from_str(&h.call(RUN_TEST, json!({})).await.content).unwrap();
    assert_eq!(first["classification"], "needs_repair");
    assert_eq!(first["runs_left"], 1);
    let second: Value = serde_json::from_str(&h.call(RUN_TEST, json!({})).await.content).unwrap();
    assert_eq!(second["runs_left"], 0);
    assert!(h.call(RUN_TEST, json!({})).await.content.contains("no test runs left"));
    assert!(h.call(WRITE_TEST, write("calc/qgate_s1_test.go")).await.content.contains("no test runs left"));

    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, f.workspace.root().join(""));
    assert_eq!(calls[0].1, f.targets[0].command);
    let facts = h.into_facts();
    assert_eq!(facts.runs.len(), 2);
    assert_eq!(facts.runs[0].source, TEST, "the source as it was run is recorded");
    assert_eq!(facts.target.unwrap().test_file, "calc/qgate_s1_test.go");
}

#[tokio::test]
async fn terminal_runs_stop_further_runs() {
    let f = fixture();
    let missing = RawRun { spawn_error: Some("not found".into()), not_found: true, ..RawRun::default() };
    let runner = FakeRunner::with(vec![missing]);
    let h = ScenarioToolHandler::new(&f.workspace, &runner, &f.targets, limits(3));
    h.call(WRITE_TEST, write("calc/qgate_s1_test.go")).await;
    assert!(h.call(RUN_TEST, json!({})).await.content.contains("toolchain_missing"));
    assert!(h.call(RUN_TEST, json!({})).await.content.contains("cannot run in this environment"));
}

#[tokio::test]
async fn report_result_verifies_code_references() {
    let f = fixture();
    let runner = FakeRunner::with(vec![failed_run(GO_ASSERTION_OUTPUT)]);
    let h = ScenarioToolHandler::new(&f.workspace, &runner, &f.targets, limits(1));
    h.call(WRITE_TEST, write("calc/qgate_s1_test.go")).await;
    h.call(RUN_TEST, json!({})).await;

    let invented = json!([{ "file": "calc/calc.go", "line": 4, "quote": "return a + b" }]);
    let bounced = h.call(REPORT_RESULT, report("failed_assertion", invented)).await;
    assert!(bounced.is_error && bounced.content.contains("does not contain the quote"));
    assert!(!h.finished());

    let refs = json!([
        { "file": "calc/calc.go", "line": 6, "quote": "  return a - b  " },
        { "file": "calc/qgate_s1_test.go", "line": 5, "quote": "func TestQgateS1" },
        { "file": "calc/calc.go", "line": 40, "quote": "return a - b" },
        { "file": "../x", "line": 1, "quote": "x" },
        { "file": "calc/calc.go", "line": 0, "quote": "" },
    ]);
    assert_eq!(h.call(REPORT_RESULT, report("failed_assertion", refs)).await, ToolOutput::ok("result recorded"));
    assert!(h.finished());
    assert!(h.call(REPORT_RESULT, report("passed", json!([]))).await.content.contains("already reported"));

    let report = h.into_facts().report.unwrap();
    assert_eq!(report.advisory, TestVerdict::FailedAssertion);
    assert_eq!(
        report.code_under_test,
        [EvidenceRef { file: "calc/calc.go".into(), line: 6, quote: "return a - b".into() }],
        "within ±3 lines, quote trimmed"
    );
    assert_eq!(report.rejected.len(), 4);
    assert!(report.rejected.iter().any(|r| r.contains("generated test")));
}

#[tokio::test]
async fn second_report_without_valid_references_is_accepted_empty() {
    let f = fixture();
    let runner = FakeRunner::default();
    let h = ScenarioToolHandler::new(&f.workspace, &runner, &f.targets, limits(1));
    assert!(h.call(REPORT_RESULT, report("inconclusive", json!([]))).await.is_error);
    assert!(!h.call(REPORT_RESULT, report("inconclusive", json!([]))).await.is_error);
    assert!(h.finished());
    assert!(
        h.call(REPORT_RESULT, json!({ "verdict": "maybe", "code_under_test": [], "explanation": "" })).await.is_error
    );
}

#[test]
fn tool_specs_are_strict_objects() {
    let specs = tool_specs();
    let names: Vec<_> = specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, [LIST_FILES, READ_FILE, SEARCH, WRITE_TEST, RUN_TEST, REPORT_RESULT]);
    for spec in &specs {
        assert_eq!(spec.input_schema["type"], "object");
        assert_eq!(spec.input_schema["additionalProperties"], false);
    }
}
