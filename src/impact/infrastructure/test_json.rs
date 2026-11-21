use std::collections::BTreeMap;

use serde::Deserialize;

use crate::impact::domain::{PACKAGE_LEVEL, TestOutcome, TestResult, TestRun};

const MAX_TEST_OUTPUT_BYTES: usize = 8 * 1024;

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Package")]
    package: Option<String>,
    #[serde(rename = "ImportPath")]
    import_path: Option<String>,
    #[serde(rename = "Test")]
    test: Option<String>,
    #[serde(rename = "Output")]
    output: Option<String>,
}

pub fn parse_test_json(text: &str) -> TestRun {
    let mut run = TestRun::default();
    let mut outputs: BTreeMap<(String, String), String> = BTreeMap::new();
    for event in text.lines().filter_map(|line| serde_json::from_str::<Event>(line.trim()).ok()) {
        apply(&mut run, &mut outputs, event);
    }
    run
}

fn apply(run: &mut TestRun, outputs: &mut BTreeMap<(String, String), String>, event: Event) {
    let test = event.test.clone().unwrap_or_default();
    if event.action == "build-output" {
        let package = event.import_path.as_deref().and_then(|p| p.split(' ').next()).unwrap_or_default();
        push_output(outputs, package, PACKAGE_LEVEL, event.output.as_deref());
        return;
    }
    let Some(package) = event.package.as_deref() else { return };
    match event.action.as_str() {
        "output" => push_output(outputs, package, &test, event.output.as_deref()),
        "pass" => record(run, outputs, package, &test, TestOutcome::Pass),
        "fail" => record(run, outputs, package, &test, TestOutcome::Fail),
        "skip" => record(run, outputs, package, &test, TestOutcome::Skip),
        _ => {}
    }
}

fn push_output(outputs: &mut BTreeMap<(String, String), String>, package: &str, test: &str, text: Option<&str>) {
    let Some(text) = text else { return };
    let buffer = outputs.entry((package.to_string(), test.to_string())).or_default();
    buffer.push_str(text);
    if buffer.len() > MAX_TEST_OUTPUT_BYTES * 2 {
        let mut cut = buffer.len() - MAX_TEST_OUTPUT_BYTES;
        while !buffer.is_char_boundary(cut) {
            cut += 1;
        }
        buffer.drain(..cut);
    }
}

fn record(
    run: &mut TestRun,
    outputs: &mut BTreeMap<(String, String), String>,
    package: &str,
    test: &str,
    outcome: TestOutcome,
) {
    let output = outputs.remove(&(package.to_string(), test.to_string())).unwrap_or_default();
    let output_tail = if outcome == TestOutcome::Fail { trim_tail(output) } else { String::new() };
    run.insert(package, test, TestResult { outcome, output_tail });
}

fn trim_tail(mut text: String) -> String {
    if text.len() > MAX_TEST_OUTPUT_BYTES {
        let mut cut = text.len() - MAX_TEST_OUTPUT_BYTES;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        text.drain(..cut);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = include_str!("fixtures/go_test_mixed.jsonl");

    #[test]
    fn parses_a_real_run_with_failing_subtest_skip_and_build_failure() {
        let run = parse_test_json(MIXED);
        let calc = "example.com/m/calc";
        assert_eq!(run.outcome(calc, "TestAddOK"), Some(TestOutcome::Pass));
        assert_eq!(run.outcome(calc, "TestAdd"), Some(TestOutcome::Fail));
        assert_eq!(run.outcome(calc, "TestAdd/pos"), Some(TestOutcome::Fail));
        assert_eq!(run.outcome(calc, "TestAdd/zero"), Some(TestOutcome::Pass));
        assert_eq!(run.outcome(calc, "TestSkip"), Some(TestOutcome::Skip));
        assert_eq!(run.outcome(calc, PACKAGE_LEVEL), Some(TestOutcome::Fail));
        assert!(run.result(calc, "TestAdd/pos").unwrap().output_tail.contains("want 3 got -1"));
        assert_eq!(run.result(calc, "TestAddOK").unwrap().output_tail, "", "passing output is dropped");
    }

    #[test]
    fn build_failures_carry_compiler_output_at_package_level() {
        let run = parse_test_json(MIXED);
        let bad = run.result("example.com/m/bad", PACKAGE_LEVEL).unwrap();
        assert_eq!(bad.outcome, TestOutcome::Fail);
        assert!(bad.output_tail.contains("undefined: undefined"), "{}", bad.output_tail);
    }

    #[test]
    fn non_json_lines_and_unknown_actions_are_ignored() {
        let text = "go: downloading x\n{\"Action\":\"start\",\"Package\":\"p\"}\nnot json\n\
                    {\"Action\":\"pass\",\"Package\":\"p\",\"Test\":\"TestA\"}\n{\"Action\":\"pass\"}\n";
        let run = parse_test_json(text);
        assert_eq!(run.packages.len(), 1);
        assert_eq!(run.outcome("p", "TestA"), Some(TestOutcome::Pass));
    }

    #[test]
    fn very_long_failure_output_keeps_only_the_tail() {
        let line = "x".repeat(1000);
        let mut text = String::new();
        for i in 0..40 {
            text.push_str(&format!(
                "{{\"Action\":\"output\",\"Package\":\"p\",\"Test\":\"T\",\"Output\":\"{line}{i}\\n\"}}\n"
            ));
        }
        text.push_str("{\"Action\":\"fail\",\"Package\":\"p\",\"Test\":\"T\"}\n");
        let tail = parse_test_json(&text).result("p", "T").unwrap().output_tail.clone();
        assert!(tail.len() <= MAX_TEST_OUTPUT_BYTES);
        assert!(tail.ends_with("39\n"));
    }
}
