use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;

use super::go_list::{LIST_FORMAT, parse_package_lines};
use super::test_json::parse_test_json;
use crate::impact::application::{GoToolchain, ImpactError, TestSelection};
use crate::impact::domain::{GoPackage, TestRun};
use crate::sandbox::application::{ExecBackend, ExecRequest, ExecResult, FailureKind, MAX_OUTPUT_BYTES};

const TRUNCATION_PERCENT: usize = 98;
const ERROR_SNIPPET_CHARS: usize = 400;

#[derive(Debug, Clone, Copy)]
pub struct GoLimits {
    pub list_timeout_ms: u64,
    pub test_timeout_ms: u64,
    pub max_output_bytes: usize,
    pub batch_size: usize,
}

impl Default for GoLimits {
    fn default() -> Self {
        Self {
            list_timeout_ms: 2 * 60 * 1000,
            test_timeout_ms: 10 * 60 * 1000,
            max_output_bytes: MAX_OUTPUT_BYTES,
            batch_size: 10,
        }
    }
}

pub struct GoCli {
    backend: Arc<dyn ExecBackend>,
    limits: GoLimits,
}

impl GoCli {
    pub fn new(backend: Arc<dyn ExecBackend>) -> Self {
        Self { backend, limits: GoLimits::default() }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: GoLimits) -> Self {
        self.limits = limits;
        self
    }

    async fn go(&self, root: &Path, args: Vec<String>, timeout_ms: u64) -> Result<ExecResult, ImpactError> {
        let mut argv = vec!["go".to_string()];
        argv.extend(args);
        let request = ExecRequest {
            argv,
            cwd: root.to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms,
            max_output_bytes: self.limits.max_output_bytes,
        };
        let result = self.backend.execute(request).await;
        match &result.failure {
            Some(f) if matches!(f.kind, FailureKind::NotFound | FailureKind::Unavailable) => {
                Err(ImpactError::ToolchainUnavailable(f.message.clone()))
            }
            Some(f) => Err(ImpactError::Failed(f.message.clone())),
            None => Ok(result),
        }
    }

    async fn test_command(&self, root: &Path, args: Vec<String>) -> Result<TestRun, ImpactError> {
        let mut argv: Vec<String> = ["test", "-json", "-count=1"].map(String::from).to_vec();
        argv.extend(args);
        let result = self.go(root, argv, self.limits.test_timeout_ms).await?;
        let mut run = parse_test_json(&result.output_tail);
        if run.packages.is_empty() && !result.success() {
            return Err(ImpactError::Failed(format!("go test produced no results: {}", snippet(&result.output_tail))));
        }
        run.incomplete = result.timed_out || self.looks_truncated(&result);
        Ok(run)
    }

    fn looks_truncated(&self, result: &ExecResult) -> bool {
        result.output_tail.len() >= self.limits.max_output_bytes / 100 * TRUNCATION_PERCENT
    }
}

impl GoToolchain for GoCli {
    fn is_go_module(&self, root: &Path) -> bool {
        root.join("go.mod").is_file()
    }

    fn list_packages<'a>(&'a self, root: &'a Path) -> BoxFuture<'a, Result<Vec<GoPackage>, ImpactError>> {
        Box::pin(async move {
            let args = ["list", "-e", "-f", LIST_FORMAT, "./..."].map(String::from).to_vec();
            let result = self.go(root, args, self.limits.list_timeout_ms).await?;
            if result.timed_out || self.looks_truncated(&result) {
                return Err(ImpactError::Failed("go list timed out or its output was too large".into()));
            }
            let packages = parse_package_lines(&result.output_tail);
            if packages.is_empty() {
                return Err(ImpactError::Failed(format!(
                    "go list found no packages: {}",
                    snippet(&result.output_tail)
                )));
            }
            Ok(packages)
        })
    }

    fn run_tests<'a>(
        &'a self,
        root: &'a Path,
        selections: &'a [TestSelection],
    ) -> BoxFuture<'a, Result<TestRun, ImpactError>> {
        Box::pin(async move {
            let mut run = TestRun::default();
            let (whole, filtered): (Vec<&TestSelection>, Vec<&TestSelection>) =
                selections.iter().partition(|s| s.tests.is_empty());
            for batch in whole.chunks(self.limits.batch_size.max(1)) {
                let packages = batch.iter().map(|s| s.package.clone()).collect();
                run.merge(self.test_command(root, packages).await?);
            }
            for selection in filtered {
                let args = vec!["-run".to_string(), run_pattern(&selection.tests), selection.package.clone()];
                run.merge(self.test_command(root, args).await?);
            }
            Ok(run)
        })
    }
}

fn run_pattern(tests: &[String]) -> String {
    let names: Vec<String> = tests.iter().map(|t| regex_escape(t)).collect();
    format!("^({})$", names.join("|"))
}

fn regex_escape(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, c| {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

fn snippet(output: &str) -> String {
    output.trim().chars().take(ERROR_SNIPPET_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::impact::domain::TestOutcome;

    struct Scripted {
        results: Mutex<Vec<ExecResult>>,
        requests: Mutex<Vec<ExecRequest>>,
    }

    impl Scripted {
        fn new(results: Vec<ExecResult>) -> Arc<Self> {
            Arc::new(Self { results: Mutex::new(results), requests: Mutex::default() })
        }
    }

    impl ExecBackend for Scripted {
        fn execute(&self, request: ExecRequest) -> BoxFuture<'_, ExecResult> {
            self.requests.lock().unwrap().push(request);
            let result = self.results.lock().unwrap().remove(0);
            Box::pin(async move { result })
        }
    }

    fn output(text: &str, exit_code: i32) -> ExecResult {
        ExecResult { exit_code: Some(exit_code), output_tail: text.into(), ..ExecResult::default() }
    }

    fn pass_event(package: &str) -> String {
        format!("{{\"Action\":\"pass\",\"Package\":\"{package}\",\"Test\":\"TestA\"}}\n")
    }

    fn selection(package: &str, tests: &[&str]) -> TestSelection {
        TestSelection { package: package.into(), tests: tests.iter().map(|t| t.to_string()).collect() }
    }

    #[tokio::test]
    async fn lists_packages_through_the_backend() {
        let backend = Scripted::new(vec![output("m/a|m|1|0|m/b|testing|\nm/b|m|0|0|||\n", 0)]);
        let go = GoCli::new(backend.clone());
        let packages = go.list_packages(Path::new("/snap")).await.unwrap();
        assert_eq!(packages.len(), 2);
        let request = &backend.requests.lock().unwrap()[0];
        assert_eq!(request.argv[..4], ["go", "list", "-e", "-f"]);
        assert_eq!(request.cwd, Path::new("/snap"));
    }

    #[tokio::test]
    async fn list_failures_map_to_errors() {
        let missing = ExecResult::failed(FailureKind::NotFound, "go: not found");
        let err = GoCli::new(Scripted::new(vec![missing])).list_packages(Path::new("/s")).await.unwrap_err();
        assert!(matches!(err, ImpactError::ToolchainUnavailable(_)));

        let rejected = ExecResult::failed(FailureKind::Rejected, "outside");
        let err = GoCli::new(Scripted::new(vec![rejected])).list_packages(Path::new("/s")).await.unwrap_err();
        assert!(matches!(err, ImpactError::Failed(m) if m == "outside"));

        let err = GoCli::new(Scripted::new(vec![output("go: go.mod broken\n", 1)]))
            .list_packages(Path::new("/s"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("go.mod broken"));

        let timed_out = ExecResult { timed_out: true, ..ExecResult::default() };
        assert!(GoCli::new(Scripted::new(vec![timed_out])).list_packages(Path::new("/s")).await.is_err());
    }

    #[tokio::test]
    async fn whole_packages_are_batched_and_filtered_ones_use_run() {
        let backend = Scripted::new(vec![
            output(&pass_event("a"), 0),
            output(&pass_event("c"), 0),
            output("{\"Action\":\"fail\",\"Package\":\"d\",\"Test\":\"TestX\"}\n", 1),
        ]);
        let limits = GoLimits { batch_size: 2, ..GoLimits::default() };
        let go = GoCli::new(backend.clone()).with_limits(limits);
        let selections =
            [selection("a", &[]), selection("b", &[]), selection("c", &[]), selection("d", &["TestX", "TestY"])];
        let run = go.run_tests(Path::new("/s"), &selections).await.unwrap();
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests[0].argv[1..], ["test", "-json", "-count=1", "a", "b"]);
        assert_eq!(requests[1].argv[4..], ["c"]);
        assert_eq!(requests[2].argv[4..], ["-run", "^(TestX|TestY)$", "d"]);
        assert_eq!(run.outcome("a", "TestA"), Some(TestOutcome::Pass));
        assert_eq!(run.outcome("d", "TestX"), Some(TestOutcome::Fail));
        assert!(!run.incomplete);
    }

    #[tokio::test]
    async fn timeouts_and_truncation_mark_the_run_incomplete() {
        let timed_out = ExecResult { timed_out: true, output_tail: pass_event("a"), ..ExecResult::default() };
        let go = GoCli::new(Scripted::new(vec![timed_out]));
        assert!(go.run_tests(Path::new("/s"), &[selection("a", &[])]).await.unwrap().incomplete);

        let limits = GoLimits { max_output_bytes: 100, ..GoLimits::default() };
        let big = output(&format!("{}{}", pass_event("a"), " ".repeat(100)), 0);
        let go = GoCli::new(Scripted::new(vec![big])).with_limits(limits);
        assert!(go.run_tests(Path::new("/s"), &[selection("a", &[])]).await.unwrap().incomplete);
    }

    #[tokio::test]
    async fn no_results_on_a_failed_command_is_an_error() {
        let go = GoCli::new(Scripted::new(vec![output("go: cannot find module\n", 1)]));
        let err = go.run_tests(Path::new("/s"), &[selection("a", &[])]).await.unwrap_err();
        assert!(err.to_string().contains("cannot find module"));
    }

    #[test]
    fn go_module_detection_and_run_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let go = GoCli::new(Scripted::new(vec![]));
        assert!(!go.is_go_module(dir.path()));
        std::fs::write(dir.path().join("go.mod"), "module m\n").unwrap();
        assert!(go.is_go_module(dir.path()));
        assert_eq!(run_pattern(&["Test.A".into()]), "^(Test\\.A)$");
    }
}

#[cfg(test)]
mod real_go_tests {
    use super::*;
    use crate::impact::application::{DEFAULT_MAX_PACKAGES, DetectRegressions, DetectRequest};
    use crate::sandbox::infrastructure::LocalBackend;

    fn go_installed() -> bool {
        std::process::Command::new("go").arg("version").output().is_ok_and(|o| o.status.success())
    }

    fn write_module(root: &Path, add_body: &str) {
        let files = [
            ("go.mod", "module example.com/m\n\ngo 1.21\n".to_string()),
            ("calc/calc.go", format!("package calc\n\nfunc Add(a, b int) int {{ {add_body} }}\n")),
            (
                "calc/calc_test.go",
                "package calc\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\tif Add(1, 2) != 3 {\n\t\tt.Fatal(\"bad add\")\n\t}\n}\n"
                    .to_string(),
            ),
            ("app/app.go", "package app\n\nimport \"example.com/m/calc\"\n\nfunc Sum() int { return calc.Add(2, 2) }\n".to_string()),
            ("app/app_test.go", "package app\n\nimport \"testing\"\n\nfunc TestSum(t *testing.T) {\n\tif Sum() != 4 {\n\t\tt.Fatal(\"bad sum\")\n\t}\n}\n".to_string()),
            ("other/other_test.go", "package other\n\nimport \"testing\"\n\nfunc TestOther(t *testing.T) { t.Fatal(\"always\") }\n".to_string()),
        ];
        for (path, content) in files {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
    }

    #[tokio::test]
    async fn detects_a_regression_in_a_dependent_package_with_real_go() {
        if !go_installed() {
            eprintln!("go not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (base, head) = (dir.path().join("base"), dir.path().join("head"));
        write_module(&base, "return a + b");
        write_module(&head, "return a - b");
        let detect = DetectRegressions::new(Arc::new(GoCli::new(Arc::new(LocalBackend))));
        let request = DetectRequest {
            base_sha: "b".into(),
            head_sha: "h".into(),
            base_snapshot: base,
            head_snapshot: head,
            changed_files: vec!["calc/calc.go".into()],
            max_packages: DEFAULT_MAX_PACKAGES,
        };
        let report = detect.execute(&request).await;

        let found: Vec<(&str, &str)> =
            report.regressions.iter().map(|r| (r.package.as_str(), r.test.as_str())).collect();
        assert_eq!(found, vec![("example.com/m/app", "TestSum"), ("example.com/m/calc", "TestAdd")]);
        assert!(report.regressions[1].evidence.head_output_tail.contains("bad add"));
        assert!(!report.packages_tested.iter().any(|p| p.ends_with("/other")), "unimpacted package is not run");
        assert!(report.blocks());
    }
}
