use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;

use super::common::{SuiteExec, finish, is_dir, is_file, line_of, read_source, shell_quote, strings, unique_name};
use super::junit::parse_junit;
use crate::impact::application::{ImpactError, TestSuite};
use crate::impact::domain::{PACKAGE_LEVEL, TestKey, TestRun};
use crate::sandbox::application::ExecBackend;

const VENV: &str = ".qgate-venv";
const REQUIREMENT_FILES: &[&str] = &["requirements.txt", "requirements-dev.txt", "requirements-test.txt"];
const CONFIG_MARKERS: &[(&str, &str)] =
    &[("pyproject.toml", "[tool.pytest"), ("setup.cfg", "[tool:pytest]"), ("tox.ini", "[pytest]")];
const MAX_REPORT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Environment {
    Uv,
    Venv(Vec<String>),
    System,
}

pub struct PytestSuite {
    exec: SuiteExec,
}

impl PytestSuite {
    pub fn new(backend: Arc<dyn ExecBackend>) -> Self {
        Self { exec: SuiteExec::new(backend) }
    }

    fn environment(root: &Path) -> Environment {
        if is_file(&root.join("uv.lock")) {
            return Environment::Uv;
        }
        let requirements: Vec<String> =
            REQUIREMENT_FILES.iter().filter(|f| is_file(&root.join(f))).map(|f| (*f).to_string()).collect();
        if requirements.is_empty() { Environment::System } else { Environment::Venv(requirements) }
    }

    async fn prepare(&self, root: &Path, environment: &Environment) -> Result<(), ImpactError> {
        match environment {
            Environment::Uv => self.exec.install(root, strings(&["uv", "sync", "--frozen"])).await,
            Environment::Venv(requirements) if !is_dir(&root.join(VENV)) => {
                self.exec.install(root, strings(&["python3", "-m", "venv", VENV])).await?;
                let mut pip =
                    vec![format!("{VENV}/bin/python"), "-m".into(), "pip".into(), "install".into(), "-q".into()];
                for file in requirements {
                    pip.extend(["-r".to_string(), file.clone()]);
                }
                pip.push("pytest".into());
                self.exec.install(root, pip).await
            }
            Environment::Venv(_) | Environment::System => Ok(()),
        }
    }

    fn python(environment: &Environment) -> Vec<String> {
        match environment {
            Environment::Uv => strings(&["uv", "run", "--frozen", "--no-sync", "python"]),
            Environment::Venv(_) => vec![format!("{VENV}/bin/python")],
            Environment::System => strings(&["python3"]),
        }
    }
}

impl TestSuite for PytestSuite {
    fn name(&self) -> &'static str {
        "pytest"
    }

    fn detect(&self, root: &Path) -> bool {
        is_file(&root.join("pytest.ini"))
            || is_file(&root.join("conftest.py"))
            || CONFIG_MARKERS
                .iter()
                .any(|(file, marker)| read_source(&root.join(file)).is_some_and(|text| text.contains(marker)))
    }

    fn run<'a>(&'a self, root: &'a Path, only: &'a [TestKey]) -> BoxFuture<'a, Result<TestRun, ImpactError>> {
        Box::pin(async move {
            let environment = Self::environment(root);
            if only.is_empty() {
                self.prepare(root, &environment).await?;
            }
            let report = unique_name("pytest", "xml");
            let mut argv = Self::python(&environment);
            argv.extend(strings(&["-m", "pytest", "-q", "-p", "no:cacheprovider", "-o", "junit_family=xunit1"]));
            argv.push(format!("--junitxml={report}"));
            argv.extend(node_ids(root, only).unwrap_or_default());
            let result = self.exec.run(root, argv).await?;
            let mut run = TestRun::default();
            if let Some(xml) = super::common::read_small(&root.join(&report), MAX_REPORT_BYTES) {
                parse_junit(&xml, &mut run).map_err(ImpactError::Failed)?;
            }
            finish(run, &result)
        })
    }

    fn reproduce_command(&self, root: &Path, key: &TestKey) -> String {
        let python = Self::python(&Self::environment(root)).join(" ");
        if key.test == PACKAGE_LEVEL {
            return format!("{python} -m pytest");
        }
        let target = node_id(root, key).unwrap_or_else(|| format!("{}::{}", key.package, key.test));
        format!("{python} -m pytest {}", shell_quote(&target))
    }

    fn locate(&self, root: &Path, key: &TestKey) -> Option<(String, u32)> {
        let (file, _) = module_file(root, &key.package)?;
        let function = key.test.split('[').next().unwrap_or(&key.test);
        let line = line_of(&root.join(&file), &format!("def {function}")).unwrap_or(1);
        Some((file, line))
    }
}

fn node_ids(root: &Path, only: &[TestKey]) -> Option<Vec<String>> {
    if only.is_empty() || only.iter().any(|k| k.test == PACKAGE_LEVEL) {
        return None;
    }
    only.iter().map(|k| node_id(root, k)).collect()
}

fn node_id(root: &Path, key: &TestKey) -> Option<String> {
    let (file, classes) = module_file(root, &key.package)?;
    let mut parts = vec![file];
    parts.extend(classes);
    parts.push(key.test.clone());
    Some(parts.join("::"))
}

fn module_file(root: &Path, classname: &str) -> Option<(String, Vec<String>)> {
    let segments: Vec<&str> = classname.split('.').filter(|s| !s.is_empty()).collect();
    (1..=segments.len()).rev().find_map(|split| {
        let file = format!("{}.py", segments[..split].join("/"));
        is_file(&root.join(&file)).then(|| (file, segments[split..].iter().map(|s| (*s).to_string()).collect()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::domain::TestOutcome;
    use crate::sandbox::application::fakes::RecordingBackend;
    use crate::sandbox::application::{ExecRequest, ExecResult};

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn suite() -> (PytestSuite, Arc<RecordingBackend>) {
        let backend = Arc::new(RecordingBackend::answering(ExecResult { exit_code: Some(1), ..ExecResult::default() }));
        (PytestSuite::new(backend.clone()), backend)
    }

    fn report_path(request: &ExecRequest) -> String {
        request.argv.iter().find_map(|a| a.strip_prefix("--junitxml=")).unwrap().to_string()
    }

    #[test]
    fn detects_pytest_configuration() {
        let root = tempfile::tempdir().unwrap();
        assert!(!suite().0.detect(root.path()));
        write(root.path(), "pyproject.toml", "[project]\nname='x'\n");
        assert!(!suite().0.detect(root.path()));
        write(root.path(), "pyproject.toml", "[tool.pytest.ini_options]\n");
        assert!(suite().0.detect(root.path()));
    }

    #[test]
    fn node_ids_resolve_module_files_and_classes() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "tests/test_cart.py", "class TestTax:\n    def test_rate(self):\n        pass\n");
        let key = TestKey::new("tests.test_cart.TestTax", "test_rate[2]");
        assert_eq!(node_id(root.path(), &key).as_deref(), Some("tests/test_cart.py::TestTax::test_rate[2]"));
        assert_eq!(suite().0.locate(root.path(), &key), Some(("tests/test_cart.py".into(), 2)));
        assert_eq!(node_ids(root.path(), &[TestKey::new("missing", "t")]), None);
        assert_eq!(node_ids(root.path(), &[TestKey::new("pytest", PACKAGE_LEVEL)]), None);
        assert_eq!(
            suite().0.reproduce_command(root.path(), &key),
            "python3 -m pytest 'tests/test_cart.py::TestTax::test_rate[2]'"
        );
    }

    #[tokio::test]
    async fn requirements_project_installs_into_a_venv_once_then_runs_with_it() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "pytest.ini", "");
        write(root.path(), "requirements.txt", "requests\n");
        let (suite, backend) = suite();
        let error = suite.run(root.path(), &[]).await.unwrap_err();
        assert!(matches!(error, ImpactError::Failed(m) if m.contains("python3 -m venv")), "install failure surfaces");
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests[0].argv, strings(&["python3", "-m", "venv", VENV]));
    }

    #[tokio::test]
    async fn run_reads_its_own_junit_report() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "conftest.py", "");
        write(root.path(), "uv.lock", "");
        let backend = Arc::new(RecordingBackend::answering(ExecResult { exit_code: Some(0), ..ExecResult::default() }));
        let suite = PytestSuite::new(backend.clone());
        assert!(matches!(suite.run(root.path(), &[]).await, Err(ImpactError::NoResults(_))));
        let report = report_path(&backend.requests.lock().unwrap()[1]);
        write(
            root.path(),
            &report,
            r#"<testsuite><testcase classname="t" name="test_a"><failure/></testcase></testsuite>"#,
        );
        let rerun_report = {
            let suite = PytestSuite::new(Arc::new(StaticReport { report: report.clone() }));
            suite.run(root.path(), &[TestKey::new("t", "test_a")]).await.unwrap()
        };
        assert_eq!(rerun_report.outcome("t", "test_a"), Some(TestOutcome::Fail));
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests[0].argv, strings(&["uv", "sync", "--frozen"]));
        assert_eq!(&requests[1].argv[..5], &strings(&["uv", "run", "--frozen", "--no-sync", "python"])[..]);
    }

    struct StaticReport {
        report: String,
    }

    impl crate::sandbox::application::ExecBackend for StaticReport {
        fn execute(&self, request: ExecRequest) -> BoxFuture<'_, ExecResult> {
            let written = report_path(&request);
            let source = request.cwd.join(&self.report);
            std::fs::copy(&source, request.cwd.join(written)).unwrap();
            Box::pin(async { ExecResult { exit_code: Some(1), ..ExecResult::default() } })
        }
    }
}
