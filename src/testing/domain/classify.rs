//! Classifies one run of the generated test from facts only (exit code, timeout, output).

use super::language::TestLanguage;

/// What the process runner observed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawRun {
    pub exit_code: Option<i32>,
    /// Interleaved stdout/stderr tail.
    pub output: String,
    pub timed_out: bool,
    /// The program could not be started; `true` in `not_found` when it does not exist.
    pub spawn_error: Option<String>,
    pub not_found: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunClass {
    Passed,
    /// The test ran and an assertion failed.
    FailedAssertion,
    /// Compile or setup error, or a failure without an assertion: the test must be fixed.
    NeedsRepair,
    TimedOut,
    /// The language toolchain (or test runner) is not installed.
    ToolchainMissing,
    /// The command could not be started for another reason (e.g. wrapper not executable).
    SetupFailed,
}

impl RunClass {
    /// Further runs cannot change the outcome.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::ToolchainMissing | Self::SetupFailed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::FailedAssertion => "failed_assertion",
            Self::NeedsRepair => "needs_repair",
            Self::TimedOut => "timed_out",
            Self::ToolchainMissing => "toolchain_missing",
            Self::SetupFailed => "setup_failed",
        }
    }
}

const TOOLCHAIN_MARKERS: &[&str] = &[
    "command not found",
    "No module named pytest",
    "JAVA_HOME is not",
    "JAVA_HOME environment variable is not defined",
];

pub fn classify(language: TestLanguage, run: &RawRun) -> RunClass {
    if run.spawn_error.is_some() {
        return if run.not_found { RunClass::ToolchainMissing } else { RunClass::SetupFailed };
    }
    if run.timed_out {
        return RunClass::TimedOut;
    }
    if run.exit_code == Some(0) {
        return RunClass::Passed;
    }
    let output = run.output.as_str();
    if TOOLCHAIN_MARKERS.iter().any(|m| output.contains(m)) {
        return RunClass::ToolchainMissing;
    }
    // Assertion markers are specific enough that no compile/setup failure produces them; anything
    // else (compile error, import error, panic without assertion, no test found) needs repair.
    if has_assertion_failure(language, output) { RunClass::FailedAssertion } else { RunClass::NeedsRepair }
}

fn has_assertion_failure(language: TestLanguage, output: &str) -> bool {
    match language {
        TestLanguage::Go => output.contains("--- FAIL") && output.lines().any(is_go_test_log_line),
        TestLanguage::Java => {
            ["AssertionFailedError", "expected:<", "java.lang.AssertionError"].iter().any(|m| output.contains(m))
        }
        TestLanguage::Python => output.contains("AssertionError") || output.lines().any(is_pytest_assert_line),
        TestLanguage::Rust => output.contains("panicked at") && output.contains("assertion"),
    }
}

/// `    qgate_s1_test.go:12: message` as printed by `t.Errorf` / `t.Fatalf`.
fn is_go_test_log_line(line: &str) -> bool {
    line.split_once("_test.go:").is_some_and(|(_, rest)| {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with(':')
    })
}

/// `E       assert 2 == 3` as printed by pytest's assertion rewriting.
fn is_pytest_assert_line(line: &str) -> bool {
    line.strip_prefix('E')
        .is_some_and(|rest| rest.starts_with(char::is_whitespace) && rest.trim_start().starts_with("assert "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(output: &str) -> RawRun {
        RawRun { exit_code: Some(1), output: output.into(), ..RawRun::default() }
    }

    const GO_ASSERT: &str =
        "--- FAIL: TestQgateS1 (0.00s)\n    qgate_s1_test.go:9: Add(2,3) = -1, want 5\nFAIL\nFAIL\texample/calc\t0.1s";
    const GO_BUILD: &str = "# example/calc\n./qgate_s1_test.go:6:9: undefined: Sum\nFAIL\texample/calc [build failed]";
    const GO_PANIC: &str =
        "--- FAIL: TestQgateS1 (0.00s)\npanic: runtime error: index out of range\n\t/x/qgate_s1_test.go:7 +0x1d";
    const JUNIT5: &str =
        "[ERROR] QgateS1Test.total:12 org.opentest4j.AssertionFailedError: expected: <110> but was: <100>";
    const JUNIT4: &str = "testTotal(QgateS1Test): expected:<110> but was:<100>";
    const JAVA_COMPILE: &str = "[ERROR] COMPILATION ERROR :\n[ERROR] QgateS1Test.java:[5,9] cannot find symbol";
    const PYTEST_ASSERT: &str =
        "F\n    def test_total():\n>       assert total(2) == 3\nE       assert 2 == 3\n1 failed";
    const PYTEST_IMPORT: &str =
        "ImportError while importing test module\nE   ModuleNotFoundError: No module named 'shop'";
    const RUST_ASSERT: &str =
        "thread 'total' panicked at tests/qgate_s1.rs:5:5:\nassertion `left == right` failed\n  left: 1";
    const RUST_COMPILE: &str = "error[E0425]: cannot find function `total` in crate `shop`";

    #[test]
    fn exit_zero_passes() {
        let run = RawRun { exit_code: Some(0), output: "ok".into(), ..RawRun::default() };
        for language in TestLanguage::ALL {
            assert_eq!(classify(language, &run), RunClass::Passed);
        }
    }

    #[test]
    fn assertion_failures_per_language() {
        assert_eq!(classify(TestLanguage::Go, &failed(GO_ASSERT)), RunClass::FailedAssertion);
        assert_eq!(classify(TestLanguage::Java, &failed(JUNIT5)), RunClass::FailedAssertion);
        assert_eq!(classify(TestLanguage::Java, &failed(JUNIT4)), RunClass::FailedAssertion);
        assert_eq!(classify(TestLanguage::Python, &failed(PYTEST_ASSERT)), RunClass::FailedAssertion);
        assert_eq!(classify(TestLanguage::Python, &failed("raise AssertionError('x')")), RunClass::FailedAssertion);
        assert_eq!(classify(TestLanguage::Rust, &failed(RUST_ASSERT)), RunClass::FailedAssertion);
    }

    #[test]
    fn compile_and_setup_errors_need_repair() {
        assert_eq!(classify(TestLanguage::Go, &failed(GO_BUILD)), RunClass::NeedsRepair);
        assert_eq!(classify(TestLanguage::Go, &failed(GO_PANIC)), RunClass::NeedsRepair);
        assert_eq!(classify(TestLanguage::Java, &failed(JAVA_COMPILE)), RunClass::NeedsRepair);
        assert_eq!(classify(TestLanguage::Python, &failed(PYTEST_IMPORT)), RunClass::NeedsRepair);
        assert_eq!(classify(TestLanguage::Rust, &failed(RUST_COMPILE)), RunClass::NeedsRepair);
        let killed = RawRun { exit_code: None, output: "killed".into(), ..RawRun::default() };
        assert_eq!(classify(TestLanguage::Go, &killed), RunClass::NeedsRepair);
    }

    #[test]
    fn markers_of_another_language_do_not_count() {
        assert_eq!(classify(TestLanguage::Go, &failed(RUST_ASSERT)), RunClass::NeedsRepair);
        assert_eq!(classify(TestLanguage::Rust, &failed(GO_ASSERT)), RunClass::NeedsRepair);
    }

    #[test]
    fn timeouts_and_missing_toolchains() {
        let timed_out = RawRun { timed_out: true, ..RawRun::default() };
        assert_eq!(classify(TestLanguage::Go, &timed_out), RunClass::TimedOut);
        let missing = RawRun { spawn_error: Some("No such file".into()), not_found: true, ..RawRun::default() };
        assert_eq!(classify(TestLanguage::Rust, &missing), RunClass::ToolchainMissing);
        let denied = RawRun { spawn_error: Some("Permission denied".into()), ..RawRun::default() };
        assert_eq!(classify(TestLanguage::Java, &denied), RunClass::SetupFailed);
        let no_pytest = failed("/usr/bin/python: No module named pytest");
        assert_eq!(classify(TestLanguage::Python, &no_pytest), RunClass::ToolchainMissing);
        let no_java = failed("ERROR: JAVA_HOME is not set and no 'java' command could be found");
        assert_eq!(classify(TestLanguage::Java, &no_java), RunClass::ToolchainMissing);
        assert!(RunClass::ToolchainMissing.is_terminal() && RunClass::SetupFailed.is_terminal());
        assert!(!RunClass::NeedsRepair.is_terminal());
        assert_eq!(RunClass::FailedAssertion.as_str(), "failed_assertion");
    }
}
