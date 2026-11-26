use std::path::Path;
use std::sync::Arc;

use super::all_suites;
use crate::impact::application::{DetectRequest, DetectSuiteRegressions};
use crate::impact::domain::{PACKAGE_LEVEL, RegressionReport};
use crate::sandbox::application::ExecBackend;
use crate::sandbox::infrastructure::LocalBackend;

fn write(root: &Path, files: &[(&str, &str)]) {
    for (rel, content) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

async fn detect(base: &[(&str, &str)], head: &[(&str, &str)]) -> RegressionReport {
    let work = tempfile::tempdir().unwrap();
    let (base_dir, head_dir) = (work.path().join("base"), work.path().join("head"));
    write(&base_dir, base);
    write(&head_dir, head);
    let backend: Arc<dyn ExecBackend> = Arc::new(LocalBackend);
    let request = DetectRequest {
        base_sha: "base".into(),
        head_sha: "head".into(),
        base_snapshot: base_dir,
        head_snapshot: head_dir,
        changed_files: vec![],
        max_packages: 50,
    };
    DetectSuiteRegressions::new(all_suites(&backend)).execute(&request).await.expect("a suite is detected")
}

const CARGO_TOML: &str = "[package]\nname = \"calc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const RUST_TESTS: &str = "\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn adds() { assert_eq!(add(2, 2), 4); }\n    #[test]\n    fn stable() { assert!(true); }\n}\n";

#[tokio::test]
#[ignore = "runs the real cargo toolchain; `cargo test -- --ignored live_suite`"]
async fn live_suite_cargo_detects_a_broken_test_and_a_broken_build() {
    let good = format!("pub fn add(a: i32, b: i32) -> i32 {{ a + b }}\n{RUST_TESTS}");
    let wrong = format!("pub fn add(a: i32, b: i32) -> i32 {{ a - b }}\n{RUST_TESTS}");
    let report = detect(
        &[("Cargo.toml", CARGO_TOML), ("src/lib.rs", &good)],
        &[("Cargo.toml", CARGO_TOML), ("src/lib.rs", &wrong)],
    )
    .await;
    assert_eq!(report.regressions.len(), 1, "{report:#?}");
    let regression = &report.regressions[0];
    assert_eq!(regression.test, "tests::adds");
    assert!(regression.evidence.head_output_tail.contains("left: 0"), "{}", regression.evidence.head_output_tail);
    assert_eq!(regression.evidence.reproduce_command, "cargo test --workspace -- --exact tests::adds");
    assert_eq!(regression.location, Some(("src/lib.rs".into(), 7)));

    let broken = format!("pub fn add(a: i32, b: i32) -> i32 {{ a + }}\n{RUST_TESTS}");
    let report = detect(
        &[("Cargo.toml", CARGO_TOML), ("src/lib.rs", &good)],
        &[("Cargo.toml", CARGO_TOML), ("src/lib.rs", &broken)],
    )
    .await;
    assert_eq!(report.regressions.len(), 1, "{report:#?}");
    assert_eq!(report.regressions[0].test, PACKAGE_LEVEL);
    assert!(report.notes[0].contains("build or setup broken"));
}

#[tokio::test]
#[ignore = "creates a venv and installs pytest from PyPI; `cargo test -- --ignored live_suite`"]
async fn live_suite_pytest_in_a_fresh_venv_detects_a_broken_test() {
    let tests = "from calc import add\n\n\ndef test_add():\n    assert add(2, 2) == 4\n\n\ndef test_stable():\n    assert True\n";
    let common = [("pytest.ini", "[pytest]\n"), ("requirements.txt", "\n"), ("tests/test_calc.py", tests)];
    let mut base = common.to_vec();
    base.push(("calc.py", "def add(a, b):\n    return a + b\n"));
    let mut head = common.to_vec();
    head.push(("calc.py", "def add(a, b):\n    return a * b + 1\n"));
    let report = detect(&base, &head).await;
    assert_eq!(report.regressions.len(), 1, "{report:#?}");
    let regression = &report.regressions[0];
    assert_eq!((regression.package.as_str(), regression.test.as_str()), ("tests.test_calc", "test_add"));
    assert_eq!(regression.location, Some(("tests/test_calc.py".into(), 4)));
    assert!(regression.evidence.reproduce_command.ends_with("-m pytest tests/test_calc.py::test_add"));
}

fn node_project(runner: &str, version: &str, module_type: &str) -> (String, String) {
    let template = tempfile::tempdir().unwrap();
    let manifest = format!(
        r#"{{"name": "calc", "version": "1.0.0", "type": "{module_type}", "devDependencies": {{"{runner}": "{version}"}}}}"#
    );
    std::fs::write(template.path().join("package.json"), &manifest).unwrap();
    let status = std::process::Command::new("npm")
        .args(["install", "--package-lock-only", "--no-audit", "--no-fund"])
        .current_dir(template.path())
        .status()
        .unwrap();
    assert!(status.success());
    (manifest, std::fs::read_to_string(template.path().join("package-lock.json")).unwrap())
}

struct NodeFixture {
    runner: &'static str,
    version: &'static str,
    module_type: &'static str,
    header: &'static str,
    export: &'static str,
}

async fn live_node(fixture: NodeFixture) {
    let (manifest, lock) = node_project(fixture.runner, fixture.version, fixture.module_type);
    let test = format!(
        "{}\ndescribe('calc', () => {{\n  it('adds two numbers', () => {{\n    expect(add(2, 2)).toBe(4);\n  }});\n  it('is stable', () => {{\n    expect(true).toBe(true);\n  }});\n}});\n",
        fixture.header
    );
    let (good, wrong) =
        (format!("{} (a, b) => a + b;\n", fixture.export), format!("{} (a, b) => a * b + 1;\n", fixture.export));
    let common = [
        ("package.json", manifest.as_str()),
        ("package-lock.json", lock.as_str()),
        ("test/calc.test.js", test.as_str()),
    ];
    let mut base = common.to_vec();
    base.push(("calc.js", good.as_str()));
    let mut head = common.to_vec();
    head.push(("calc.js", wrong.as_str()));
    let report = detect(&base, &head).await;
    assert_eq!(report.regressions.len(), 1, "{report:#?}");
    let regression = &report.regressions[0];
    assert_eq!((regression.package.as_str(), regression.test.as_str()), ("test/calc.test.js", "calc adds two numbers"));
    let it_line = u32::try_from(fixture.header.lines().count()).unwrap() + 2;
    assert_eq!(regression.location, Some(("test/calc.test.js".into(), it_line)));
    assert!(regression.evidence.head_output_tail.contains('5'), "{}", regression.evidence.head_output_tail);
}

#[tokio::test]
#[ignore = "installs jest from npm; `cargo test -- --ignored live_suite`"]
async fn live_suite_jest_detects_a_broken_test() {
    live_node(NodeFixture {
        runner: "jest",
        version: "29.7.0",
        module_type: "commonjs",
        header: "const { add } = require('../calc');",
        export: "exports.add =",
    })
    .await;
}

#[tokio::test]
#[ignore = "installs vitest from npm; `cargo test -- --ignored live_suite`"]
async fn live_suite_vitest_detects_a_broken_test() {
    live_node(NodeFixture {
        runner: "vitest",
        version: "2.1.9",
        module_type: "module",
        header: "import { describe, it, expect } from 'vitest';\nimport { add } from '../calc.js';",
        export: "export const add =",
    })
    .await;
}

struct RunnerImage {
    mount: std::path::PathBuf,
}

impl ExecBackend for RunnerImage {
    fn execute(
        &self,
        request: crate::sandbox::application::ExecRequest,
    ) -> futures::future::BoxFuture<'_, crate::sandbox::application::ExecResult> {
        let mount = self.mount.display().to_string();
        let mut argv: Vec<String> = ["docker", "run", "--rm", "-e", "HOME=/tmp", "-v"].map(String::from).to_vec();
        argv.push(format!("{mount}:{mount}"));
        argv.extend(
            ["-v", "qgate_qgate-toolchains:/toolchains:ro", "-v", "qgate_qgate-runner-cache:/data/cache"]
                .map(String::from),
        );
        argv.extend([
            "-w".to_string(),
            request.cwd.display().to_string(),
            "--entrypoint".into(),
            request.argv[0].clone(),
        ]);
        argv.push("qgate-runner:local".into());
        argv.extend(request.argv[1..].iter().cloned());
        LocalBackend.execute(crate::sandbox::application::ExecRequest { argv, ..request })
    }
}

#[tokio::test]
#[ignore = "runs Maven inside the qgate-runner image; `cargo test -- --ignored live_suite_maven`"]
async fn live_suite_maven_in_the_runner_image_detects_a_broken_test() {
    let pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion><groupId>com.shop</groupId><artifactId>calc</artifactId><version>1</version>
  <properties><maven.compiler.release>17</maven.compiler.release><project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>
  <dependencies><dependency><groupId>org.junit.jupiter</groupId><artifactId>junit-jupiter</artifactId><version>5.10.2</version><scope>test</scope></dependency></dependencies>
  <build><plugins><plugin><artifactId>maven-surefire-plugin</artifactId><version>3.2.5</version></plugin></plugins></build>
</project>"#;
    let test = "package com.shop;\n\nimport org.junit.jupiter.api.Test;\nimport static org.junit.jupiter.api.Assertions.assertEquals;\n\nclass CalcTest {\n    @Test\n    void adds() { assertEquals(4, Calc.add(2, 2)); }\n\n    @Test\n    void stable() { assertEquals(1, 1); }\n}\n";
    let source = |body: &str| {
        format!(
            "package com.shop;\n\npublic class Calc {{\n    public static int add(int a, int b) {{ return {body}; }}\n}}\n"
        )
    };
    let (good, wrong) = (source("a + b"), source("a * b + 1"));
    let common = [("pom.xml", pom), ("src/test/java/com/shop/CalcTest.java", test)];
    let mut base = common.to_vec();
    base.push(("src/main/java/com/shop/Calc.java", good.as_str()));
    let mut head = common.to_vec();
    head.push(("src/main/java/com/shop/Calc.java", wrong.as_str()));

    let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let (base_dir, head_dir) = (work.path().join("base"), work.path().join("head"));
    write(&base_dir, &base);
    write(&head_dir, &head);
    let backend: Arc<dyn ExecBackend> = Arc::new(RunnerImage { mount: work.path().to_path_buf() });
    let request = DetectRequest {
        base_sha: "base".into(),
        head_sha: "head".into(),
        base_snapshot: base_dir,
        head_snapshot: head_dir,
        changed_files: vec![],
        max_packages: 50,
    };
    let report = DetectSuiteRegressions::new(all_suites(&backend)).execute(&request).await.unwrap();
    assert_eq!(report.regressions.len(), 1, "{report:#?}");
    let regression = &report.regressions[0];
    assert_eq!((regression.package.as_str(), regression.test.as_str()), ("com.shop.CalcTest", "adds"));
    assert_eq!(regression.evidence.reproduce_command, "mvn -Dtest=com.shop.CalcTest#adds test");
    assert_eq!(regression.location, Some(("src/test/java/com/shop/CalcTest.java".into(), 8)));
    assert!(
        regression.evidence.head_output_tail.contains("expected: <4> but was: <5>"),
        "{}",
        regression.evidence.head_output_tail
    );
}
