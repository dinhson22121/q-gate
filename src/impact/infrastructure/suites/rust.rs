use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;

use super::common::{SuiteExec, finish, is_file, line_of, relative, shell_quote, strings, walk_files};
use crate::impact::application::{ImpactError, TestSuite};
use crate::impact::domain::{PACKAGE_LEVEL, TestKey, TestOutcome, TestResult, TestRun};
use crate::sandbox::application::ExecBackend;

const OUTPUT_TAIL_CHARS: usize = 4000;

pub struct CargoSuite {
    exec: SuiteExec,
}

impl CargoSuite {
    pub fn new(backend: Arc<dyn ExecBackend>) -> Self {
        Self { exec: SuiteExec::new(backend) }
    }
}

impl TestSuite for CargoSuite {
    fn name(&self) -> &'static str {
        "cargo"
    }

    fn detect(&self, root: &Path) -> bool {
        is_file(&root.join("Cargo.toml"))
    }

    fn run<'a>(&'a self, root: &'a Path, only: &'a [TestKey]) -> BoxFuture<'a, Result<TestRun, ImpactError>> {
        Box::pin(async move {
            let mut argv = strings(&["cargo", "test", "--no-fail-fast", "--workspace"]);
            if !only.is_empty() && only.iter().all(|k| k.test != PACKAGE_LEVEL) {
                argv.extend(strings(&["--", "--exact"]));
                argv.extend(only.iter().map(|k| k.test.clone()));
            }
            let result = self.exec.run(root, argv).await?;
            finish(parse_libtest(&result.output_tail), &result)
        })
    }

    fn reproduce_command(&self, _root: &Path, key: &TestKey) -> String {
        if key.test == PACKAGE_LEVEL {
            return "cargo test --workspace".into();
        }
        format!("cargo test --workspace -- --exact {}", shell_quote(&key.test))
    }

    fn locate(&self, root: &Path, key: &TestKey) -> Option<(String, u32)> {
        let function = key.test.rsplit("::").next()?;
        let needle = format!("fn {function}(");
        walk_files(root, |p| p.extension().is_some_and(|e| e == "rs") && !relative(root, p).starts_with("target/"))
            .into_iter()
            .find_map(|file| line_of(&file, &needle).map(|line| (relative(root, &file), line)))
    }
}

pub fn parse_libtest(output: &str) -> TestRun {
    let mut run = TestRun::default();
    let mut binary = String::from("cargo");
    let mut capture: Option<(String, Vec<String>)> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(label) = binary_label(trimmed) {
            flush(&mut run, &binary, capture.take());
            binary = label;
        } else if let Some(name) = trimmed.strip_prefix("---- ").and_then(|r| r.strip_suffix(" stdout ----")) {
            flush(&mut run, &binary, capture.take());
            capture = Some((name.to_string(), Vec::new()));
        } else if trimmed == "failures:" || trimmed.starts_with("test result:") {
            flush(&mut run, &binary, capture.take());
        } else if let Some((name, outcome)) = test_line(trimmed) {
            run.insert(&binary, name, TestResult { outcome, output_tail: String::new() });
        } else if let Some((_, lines)) = capture.as_mut() {
            lines.push(line.to_string());
        }
    }
    flush(&mut run, &binary, capture);
    run
}

fn binary_label(line: &str) -> Option<String> {
    if let Some(name) = line.strip_prefix("Doc-tests ") {
        return Some(format!("{}: doc-tests", name.trim()));
    }
    let rest = line.strip_prefix("Running ")?;
    let (label, artifact) = rest.split_once(" (")?;
    let stem = Path::new(artifact.trim_end_matches(')')).file_stem()?.to_string_lossy().to_string();
    let krate = stem.rsplit_once('-').map_or(stem.as_str(), |(name, _)| name).to_string();
    Some(format!("{krate}: {}", label.trim()))
}

fn test_line(line: &str) -> Option<(&str, TestOutcome)> {
    let rest = line.strip_prefix("test ")?;
    let (name, verdict) = rest.rsplit_once(" ... ")?;
    let outcome = match verdict.split_whitespace().next()?.trim_end_matches(',') {
        "ok" => TestOutcome::Pass,
        "FAILED" => TestOutcome::Fail,
        "ignored" => TestOutcome::Skip,
        _ => return None,
    };
    Some((name.trim(), outcome))
}

fn flush(run: &mut TestRun, binary: &str, capture: Option<(String, Vec<String>)>) {
    let Some((name, lines)) = capture else { return };
    let text = lines.join("\n");
    let skip = text.chars().count().saturating_sub(OUTPUT_TAIL_CHARS);
    let output: String = text.trim().chars().skip(skip).collect();
    if let Some(result) = run.packages.get_mut(binary).and_then(|tests| tests.get_mut(&name)) {
        result.output_tail = output;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    const OUTPUT: &str = "   Compiling shop v0.1.0
     Running unittests src/lib.rs (target/debug/deps/shop-1a2b3c)

running 3 tests
test cart::tests::total ... ok
test cart::tests::tax ... FAILED
test cart::tests::later ... ignored, slow

failures:

---- cart::tests::tax stdout ----
thread 'cart::tests::tax' panicked at src/cart.rs:20:9:
assertion `left == right` failed
  left: 12
 right: 10

failures:
    cart::tests::tax

test result: FAILED. 1 passed; 1 failed; 1 ignored
     Running tests/api.rs (target/debug/deps/api-9f8e7d)

running 1 test
test total ... ok

test result: ok. 1 passed
   Doc-tests shop

running 1 test
test src/lib.rs - add (line 3) ... ok
";

    #[test]
    fn libtest_output_is_grouped_per_binary_with_failure_output() {
        let run = parse_libtest(OUTPUT);
        assert_eq!(run.outcome("shop: unittests src/lib.rs", "cart::tests::total"), Some(TestOutcome::Pass));
        assert_eq!(run.outcome("shop: unittests src/lib.rs", "cart::tests::later"), Some(TestOutcome::Skip));
        assert_eq!(run.outcome("api: tests/api.rs", "total"), Some(TestOutcome::Pass));
        assert_eq!(run.outcome("shop: doc-tests", "src/lib.rs - add (line 3)"), Some(TestOutcome::Pass));
        let tax = run.result("shop: unittests src/lib.rs", "cart::tests::tax").unwrap();
        assert_eq!(tax.outcome, TestOutcome::Fail);
        assert!(tax.output_tail.contains("left: 12") && tax.output_tail.contains("src/cart.rs:20:9"));
        assert!(!tax.output_tail.contains("test result"));
    }

    #[test]
    fn compile_errors_produce_no_results() {
        assert!(
            parse_libtest("error[E0425]: cannot find value `x`\nerror: could not compile `shop`").packages.is_empty()
        );
    }

    #[tokio::test]
    async fn rerun_uses_exact_names_and_a_compile_failure_is_no_results() {
        let backend = Arc::new(RecordingBackend::answering(ExecResult {
            exit_code: Some(101),
            output_tail: "error: could not compile".into(),
            ..ExecResult::default()
        }));
        let suite = CargoSuite::new(backend.clone());
        let only = [TestKey::new("shop: unittests src/lib.rs", "cart::tests::tax")];
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(suite.run(root.path(), &only).await, Err(ImpactError::NoResults(_))));
        let argv = &backend.requests.lock().unwrap()[0].argv;
        assert!(argv.ends_with(&strings(&["--", "--exact", "cart::tests::tax"])));
    }

    #[test]
    fn reproduce_and_locate() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/cart.rs"), "mod tests {\n    #[test]\n    fn tax() {}\n}\n").unwrap();
        let suite = CargoSuite::new(Arc::new(RecordingBackend::default()));
        let key = TestKey::new("shop: unittests src/lib.rs", "cart::tests::tax");
        assert_eq!(suite.reproduce_command(root.path(), &key), "cargo test --workspace -- --exact cart::tests::tax");
        assert_eq!(suite.locate(root.path(), &key), Some(("src/cart.rs".into(), 3)));
        assert_eq!(
            suite.reproduce_command(root.path(), &TestKey::new("cargo", PACKAGE_LEVEL)),
            "cargo test --workspace"
        );
    }
}
