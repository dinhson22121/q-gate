use super::language::TestLanguage;
use super::profile::TestTarget;

const FATAL_CONCURRENCY_ERRORS: &[&str] = &[
    "fatal error: concurrent map writes",
    "fatal error: concurrent map read and map write",
    "fatal error: concurrent map iteration and map write",
];
const RACE_REPORT: &str = "WARNING: DATA RACE";
const RACE_FAILED_TEST: &str = "race detected during execution of test";
const FAILED_TEST: &str = "--- FAIL";
const BUILD_FAILURE_MARKERS: &[&str] = &["[build failed]", "[setup failed]"];
const RACE_FLAG: &str = "-race";

pub const RUNTIME_RACE_NOTE: &str =
    "the Go runtime detected a data race (unsynchronised concurrent access) in the test";
pub const RACE_DETECTOR_UNAVAILABLE_NOTE: &str = "the Go race detector is unavailable (cgo or a C compiler is missing), so plain data races that do not crash are not detected";

pub fn is_go_runtime_race(output: &str) -> bool {
    if BUILD_FAILURE_MARKERS.iter().any(|marker| output.contains(marker)) {
        return false;
    }
    let fatal = FATAL_CONCURRENCY_ERRORS.iter().any(|marker| output.contains(marker));
    let reported = output.contains(RACE_REPORT) && (output.contains(RACE_FAILED_TEST) || output.contains(FAILED_TEST));
    fatal || reported
}

pub fn cgo_compiler(go_env_output: &str) -> Option<String> {
    let mut lines = go_env_output.lines().map(str::trim);
    if lines.next()? != "1" {
        return None;
    }
    lines.next()?.split_whitespace().next().map(str::to_string)
}

pub fn with_race_detector(mut target: TestTarget) -> TestTarget {
    let is_go_test = target.language == TestLanguage::Go && target.command.starts_with(&["go".into(), "test".into()]);
    if is_go_test && !target.command.iter().any(|arg| arg == RACE_FLAG) {
        target.command.insert(2, RACE_FLAG.to_string());
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP_CRASH: &str = "fatal error: concurrent map writes\n\ngoroutine 20 [running]:\nshop.(*ViewCounter).Hit(...)\n\t/x/counter.go:12\nFAIL\tshop/views\t0.01s";
    const DATA_RACE: &str = "==================\nWARNING: DATA RACE\nWrite at 0x00c0 by goroutine 8:\n==================\n--- FAIL: TestQgateS6 (0.00s)\n    testing.go:1465: race detected during execution of test\nFAIL";

    fn target(language: TestLanguage, command: &[&str]) -> TestTarget {
        TestTarget {
            language,
            module_dir: String::new(),
            test_file: "a_test.go".into(),
            command: command.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn fatal_concurrent_map_errors_are_races() {
        for kind in ["map writes", "map read and map write", "map iteration and map write"] {
            assert!(is_go_runtime_race(&format!("fatal error: concurrent {kind}\n\ngoroutine 1")));
        }
        assert!(is_go_runtime_race(MAP_CRASH));
    }

    #[test]
    fn race_detector_report_with_failed_test_is_a_race() {
        assert!(is_go_runtime_race(DATA_RACE));
        assert!(is_go_runtime_race("WARNING: DATA RACE\n    testing.go:1: race detected during execution of test"));
        assert!(is_go_runtime_race("WARNING: DATA RACE\n--- FAIL: TestQgateS1 (0.1s)"));
    }

    #[test]
    fn unrelated_output_is_not_a_race() {
        assert!(!is_go_runtime_race("--- FAIL: TestQgateS1\n    a_test.go:9: got 1, want 2"));
        assert!(!is_go_runtime_race("WARNING: DATA RACE seen in a log line only"));
        assert!(!is_go_runtime_race("fatal error: all goroutines are asleep - deadlock!"));
        assert!(!is_go_runtime_race(""));
    }

    #[test]
    fn compile_failures_are_never_races() {
        let build = format!("./a_test.go:6:9: undefined: Sum\n{MAP_CRASH}\nFAIL\tshop [build failed]");
        assert!(!is_go_runtime_race(&build));
        assert!(!is_go_runtime_race("FAIL\tshop [setup failed]\nfatal error: concurrent map writes"));
    }

    #[test]
    fn cgo_compiler_needs_cgo_enabled_and_a_compiler_name() {
        assert_eq!(cgo_compiler("1\ngcc\n").as_deref(), Some("gcc"));
        assert_eq!(cgo_compiler("1\nclang -m64\n").as_deref(), Some("clang"));
        assert_eq!(cgo_compiler("0\ngcc\n"), None);
        assert_eq!(cgo_compiler("1\n"), None);
        assert_eq!(cgo_compiler("1\n\n"), None);
        assert_eq!(cgo_compiler(""), None);
    }

    #[test]
    fn race_flag_is_inserted_once_into_go_test_commands() {
        let go = target(TestLanguage::Go, &["go", "test", "./pay", "-run", "^TestQgateS1", "-count=1"]);
        let raced = with_race_detector(go);
        assert_eq!(raced.command, ["go", "test", "-race", "./pay", "-run", "^TestQgateS1", "-count=1"]);
        assert_eq!(with_race_detector(raced.clone()), raced);
    }

    #[test]
    fn race_flag_leaves_other_commands_alone() {
        let rust = target(TestLanguage::Rust, &["cargo", "test", "-q"]);
        assert_eq!(with_race_detector(rust.clone()), rust);
        let odd = target(TestLanguage::Go, &["make", "test"]);
        assert_eq!(with_race_detector(odd.clone()), odd);
    }
}
