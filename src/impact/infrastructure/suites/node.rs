use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde::Deserialize;

use super::common::{
    SuiteExec, finish, is_dir, is_file, line_of, read_small, read_source, regex_escape, relative, shell_quote, strings,
    unique_name,
};
use crate::impact::application::{ImpactError, TestSuite};
use crate::impact::domain::{PACKAGE_LEVEL, TestKey, TestOutcome, TestResult, TestRun};
use crate::sandbox::application::ExecBackend;

const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;
const OUTPUT_TAIL_CHARS: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRunner {
    Jest,
    Vitest,
}

pub struct NodeSuite {
    runner: NodeRunner,
    exec: SuiteExec,
}

#[derive(Deserialize, Default)]
struct Manifest {
    #[serde(default)]
    dependencies: serde_json::Map<String, serde_json::Value>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct Report {
    #[serde(default, rename = "testResults")]
    files: Vec<FileResult>,
}

#[derive(Deserialize)]
struct FileResult {
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    message: String,
    #[serde(default, rename = "assertionResults")]
    assertions: Vec<Assertion>,
}

#[derive(Deserialize)]
struct Assertion {
    #[serde(rename = "fullName")]
    full_name: String,
    status: String,
    #[serde(default, rename = "failureMessages")]
    failure_messages: Vec<String>,
}

impl NodeSuite {
    pub fn new(runner: NodeRunner, backend: Arc<dyn ExecBackend>) -> Self {
        Self { runner, exec: SuiteExec::new(backend) }
    }

    fn package(&self) -> &'static str {
        match self.runner {
            NodeRunner::Jest => "jest",
            NodeRunner::Vitest => "vitest",
        }
    }

    async fn install(&self, root: &Path) -> Result<(), ImpactError> {
        if is_dir(&root.join("node_modules")) {
            return Ok(());
        }
        let argv = if is_file(&root.join("package-lock.json")) {
            strings(&["npm", "ci", "--no-audit", "--no-fund"])
        } else if is_file(&root.join("pnpm-lock.yaml")) {
            strings(&["corepack", "pnpm", "install", "--frozen-lockfile"])
        } else {
            return Err(ImpactError::Failed(
                "no package-lock.json or pnpm-lock.yaml, so dependencies cannot be installed reproducibly".into(),
            ));
        };
        self.exec.install(root, argv).await
    }

    fn argv(&self, report: &str, only: &[TestKey]) -> Vec<String> {
        let mut argv = strings(&["npx", "--no-install"]);
        match self.runner {
            NodeRunner::Jest => {
                argv.extend(strings(&["jest", "--ci", "--json"]));
                argv.push(format!("--outputFile={report}"));
            }
            NodeRunner::Vitest => {
                argv.extend(strings(&["vitest", "run", "--reporter=json"]));
                argv.push(format!("--outputFile={report}"));
            }
        }
        if only.is_empty() || only.iter().any(|k| k.test == PACKAGE_LEVEL) {
            return argv;
        }
        let mut files: Vec<String> = only.iter().map(|k| k.package.clone()).collect();
        files.sort();
        files.dedup();
        argv.extend(files);
        let names: Vec<String> = only.iter().map(|k| regex_escape(&k.test)).collect();
        argv.extend(["-t".to_string(), format!("^({})$", names.join("|"))]);
        argv
    }
}

impl TestSuite for NodeSuite {
    fn name(&self) -> &'static str {
        self.package()
    }

    fn detect(&self, root: &Path) -> bool {
        let Some(manifest) = read_source(&root.join("package.json")) else { return false };
        let manifest: Manifest = serde_json::from_str(&manifest).unwrap_or_default();
        let has = |name: &str| manifest.dependencies.contains_key(name) || manifest.dev_dependencies.contains_key(name);
        match self.runner {
            NodeRunner::Vitest => has("vitest"),
            NodeRunner::Jest => has("jest") && !has("vitest"),
        }
    }

    fn run<'a>(&'a self, root: &'a Path, only: &'a [TestKey]) -> BoxFuture<'a, Result<TestRun, ImpactError>> {
        Box::pin(async move {
            if only.is_empty() {
                self.install(root).await?;
            }
            let report = unique_name(self.package(), "json");
            let result = self.exec.run(root, self.argv(&report, only)).await?;
            let run = match read_small(&root.join(&report), MAX_REPORT_BYTES) {
                Some(json) => parse_report(root, &json)?,
                None => TestRun::default(),
            };
            finish(run, &result)
        })
    }

    fn reproduce_command(&self, _root: &Path, key: &TestKey) -> String {
        let program = match self.runner {
            NodeRunner::Jest => "npx jest",
            NodeRunner::Vitest => "npx vitest run",
        };
        if key.test == PACKAGE_LEVEL {
            return if key.package == self.package() {
                program.to_string()
            } else {
                format!("{program} {}", shell_quote(&key.package))
            };
        }
        format!("{program} {} -t {}", shell_quote(&key.package), shell_quote(&key.test))
    }

    fn locate(&self, root: &Path, key: &TestKey) -> Option<(String, u32)> {
        let path = root.join(&key.package);
        if !is_file(&path) {
            return None;
        }
        let words: Vec<&str> = key.test.split_whitespace().collect();
        let line = (1..=words.len()).rev().find_map(|n| line_of(&path, &words[words.len() - n..].join(" ")));
        Some((key.package.clone(), line.unwrap_or(1)))
    }
}

fn parse_report(root: &Path, json: &str) -> Result<TestRun, ImpactError> {
    let report: Report =
        serde_json::from_str(json).map_err(|e| ImpactError::Failed(format!("invalid test report JSON: {e}")))?;
    let mut run = TestRun::default();
    for file in report.files {
        let package = relative(root, Path::new(&file.name));
        if file.assertions.is_empty() {
            let outcome = if file.status == "failed" { TestOutcome::Fail } else { TestOutcome::Pass };
            run.insert(&package, PACKAGE_LEVEL, TestResult { outcome, output_tail: tail(&file.message) });
            continue;
        }
        for assertion in file.assertions {
            let outcome = match assertion.status.as_str() {
                "passed" => TestOutcome::Pass,
                "failed" => TestOutcome::Fail,
                _ => TestOutcome::Skip,
            };
            let output = tail(&assertion.failure_messages.join("\n"));
            run.insert(&package, assertion.full_name.trim(), TestResult { outcome, output_tail: output });
        }
    }
    Ok(run)
}

fn tail(text: &str) -> String {
    let skip = text.chars().count().saturating_sub(OUTPUT_TAIL_CHARS);
    text.chars().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn suite(runner: NodeRunner) -> (NodeSuite, Arc<RecordingBackend>) {
        let backend = Arc::new(RecordingBackend::answering(ExecResult { exit_code: Some(0), ..ExecResult::default() }));
        (NodeSuite::new(runner, backend.clone()), backend)
    }

    #[test]
    fn detection_reads_package_json_dependencies() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "package.json", r#"{"devDependencies": {"jest": "29"}}"#);
        assert!(suite(NodeRunner::Jest).0.detect(root.path()));
        assert!(!suite(NodeRunner::Vitest).0.detect(root.path()));
        write(root.path(), "package.json", r#"{"devDependencies": {"jest": "29", "vitest": "2"}}"#);
        assert!(!suite(NodeRunner::Jest).0.detect(root.path()), "vitest wins when both are present");
        assert!(suite(NodeRunner::Vitest).0.detect(root.path()));
        write(root.path(), "package.json", "not json");
        assert!(!suite(NodeRunner::Jest).0.detect(root.path()));
    }

    #[test]
    fn report_maps_assertions_and_broken_files() {
        let root = Path::new("/work/head");
        let json = r#"{"testResults": [
            {"name": "/work/head/src/cart.test.ts", "status": "failed", "assertionResults": [
                {"fullName": "cart total sums items", "status": "passed", "failureMessages": []},
                {"fullName": "cart applies tax", "status": "failed", "failureMessages": ["expected 10 got 12"]},
                {"fullName": "cart later", "status": "pending", "failureMessages": []}
            ]},
            {"name": "/work/head/src/broken.test.ts", "status": "failed", "message": "SyntaxError", "assertionResults": []}
        ]}"#;
        let run = parse_report(root, json).unwrap();
        assert_eq!(run.outcome("src/cart.test.ts", "cart total sums items"), Some(TestOutcome::Pass));
        assert_eq!(run.outcome("src/cart.test.ts", "cart applies tax"), Some(TestOutcome::Fail));
        assert_eq!(run.outcome("src/cart.test.ts", "cart later"), Some(TestOutcome::Skip));
        assert_eq!(run.result("src/cart.test.ts", "cart applies tax").unwrap().output_tail, "expected 10 got 12");
        assert_eq!(run.outcome("src/broken.test.ts", PACKAGE_LEVEL), Some(TestOutcome::Fail));
        assert!(parse_report(root, "{").is_err());
    }

    #[tokio::test]
    async fn install_requires_a_lockfile_and_rerun_filters_files_and_names() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "package.json", r#"{"devDependencies": {"vitest": "2"}}"#);
        let (vitest, backend) = suite(NodeRunner::Vitest);
        assert!(vitest.run(root.path(), &[]).await.unwrap_err().to_string().contains("cannot be installed"));
        write(root.path(), "package-lock.json", "{}");
        assert!(matches!(vitest.run(root.path(), &[]).await, Err(ImpactError::NoResults(_))));
        let only = [TestKey::new("src/a.test.ts", "a (x)"), TestKey::new("src/a.test.ts", "b")];
        let _ = vitest.run(root.path(), &only).await;
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests[0].argv, strings(&["npm", "ci", "--no-audit", "--no-fund"]));
        let rerun = &requests.last().unwrap().argv;
        assert!(rerun.ends_with(&strings(&["src/a.test.ts", "-t", r"^(a \(x\)|b)$"])), "{rerun:?}");
        assert_eq!(requests.len(), 3, "a rerun does not reinstall");
    }

    #[test]
    fn reproduce_and_locate() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "src/cart.test.ts", "describe('cart', () => {\n  it('applies tax', () => {})\n})\n");
        let (jest, _) = suite(NodeRunner::Jest);
        let key = TestKey::new("src/cart.test.ts", "cart applies tax");
        assert_eq!(jest.reproduce_command(root.path(), &key), "npx jest src/cart.test.ts -t 'cart applies tax'");
        assert_eq!(jest.reproduce_command(root.path(), &TestKey::new("jest", PACKAGE_LEVEL)), "npx jest");
        assert_eq!(jest.locate(root.path(), &key), Some(("src/cart.test.ts".into(), 2)));
        assert_eq!(jest.locate(root.path(), &TestKey::new("gone.ts", "x")), None);
    }
}
