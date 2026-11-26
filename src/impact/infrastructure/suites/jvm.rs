use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use futures::future::BoxFuture;

use super::common::{SuiteExec, finish, fresh_reports, is_file, line_of, relative, shell_quote, strings, walk_files};
use super::junit::parse_junit;
use crate::impact::application::{ImpactError, TestSuite};
use crate::impact::domain::{PACKAGE_LEVEL, TestKey, TestRun};
use crate::sandbox::application::ExecBackend;

const GRADLE_MARKERS: &[&str] = &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];
const JVM_EXTENSIONS: &[&str] = &["java", "kt", "groovy", "scala"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JvmBuild {
    Maven,
    Gradle,
}

pub struct JvmSuite {
    build: JvmBuild,
    exec: SuiteExec,
}

impl JvmSuite {
    pub fn new(build: JvmBuild, backend: Arc<dyn ExecBackend>) -> Self {
        Self { build, exec: SuiteExec::new(backend) }
    }

    fn program(&self, root: &Path) -> Vec<String> {
        let (wrapper, fallback) = match self.build {
            JvmBuild::Maven => ("mvnw", "mvn"),
            JvmBuild::Gradle => ("gradlew", "gradle"),
        };
        if is_file(&root.join(wrapper)) { strings(&["sh", wrapper]) } else { strings(&[fallback]) }
    }

    fn argv(&self, root: &Path, only: &[TestKey]) -> Vec<String> {
        let mut argv = self.program(root);
        match self.build {
            JvmBuild::Maven => {
                argv.extend(strings(&[
                    "-B",
                    "-Dmaven.test.failure.ignore=true",
                    "-DfailIfNoTests=false",
                    "-Dsurefire.failIfNoSpecifiedTests=false",
                ]));
                if !only.is_empty() {
                    argv.push(format!("-Dtest={}", maven_filter(only)));
                }
                argv.push("test".into());
            }
            JvmBuild::Gradle => argv.extend(strings(&["cleanTest", "test", "--continue", "--console=plain"])),
        }
        argv
    }

    fn is_report(&self, path: &Path) -> bool {
        let text = path.to_string_lossy().replace('\\', "/");
        let is_junit = path.extension().is_some_and(|e| e == "xml")
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("TEST-"));
        let in_reports = match self.build {
            JvmBuild::Maven => text.contains("/target/surefire-reports/"),
            JvmBuild::Gradle => text.contains("/build/test-results/"),
        };
        is_junit && in_reports
    }
}

impl TestSuite for JvmSuite {
    fn name(&self) -> &'static str {
        match self.build {
            JvmBuild::Maven => "maven",
            JvmBuild::Gradle => "gradle",
        }
    }

    fn detect(&self, root: &Path) -> bool {
        match self.build {
            JvmBuild::Maven => is_file(&root.join("pom.xml")),
            JvmBuild::Gradle => {
                !is_file(&root.join("pom.xml")) && GRADLE_MARKERS.iter().any(|m| is_file(&root.join(m)))
            }
        }
    }

    fn run<'a>(&'a self, root: &'a Path, only: &'a [TestKey]) -> BoxFuture<'a, Result<TestRun, ImpactError>> {
        Box::pin(async move {
            let started = SystemTime::now();
            let result = self.exec.run(root, self.argv(root, only)).await?;
            let mut run = TestRun::default();
            for report in fresh_reports(root, started, |p| self.is_report(p)) {
                parse_junit(&report, &mut run).map_err(ImpactError::Failed)?;
            }
            finish(run, &result)
        })
    }

    fn reproduce_command(&self, root: &Path, key: &TestKey) -> String {
        let program = self.program(root);
        let program = if program[0] == "sh" { format!("./{}", program[1]) } else { program[0].clone() };
        if key.test == PACKAGE_LEVEL {
            return format!("{program} test");
        }
        let class = key.package.as_str();
        let method = method_name(&key.test);
        match self.build {
            JvmBuild::Maven => format!("{program} -Dtest={} test", shell_quote(&format!("{class}#{method}"))),
            JvmBuild::Gradle => format!("{program} test --tests {}", shell_quote(&format!("{class}.{method}"))),
        }
    }

    fn locate(&self, root: &Path, key: &TestKey) -> Option<(String, u32)> {
        let simple = key.package.rsplit('.').next()?.split('$').next()?;
        let file = walk_files(root, |p| {
            p.file_stem().is_some_and(|s| s == simple)
                && p.extension().and_then(|e| e.to_str()).is_some_and(|e| JVM_EXTENSIONS.contains(&e))
        })
        .into_iter()
        .next()?;
        let line = line_of(&file, &method_name(&key.test)).unwrap_or(1);
        Some((relative(root, &file), line))
    }
}

fn method_name(test: &str) -> String {
    test.split(['(', '[']).next().unwrap_or(test).trim().to_string()
}

fn maven_filter(only: &[TestKey]) -> String {
    let mut filters: Vec<String> = only
        .iter()
        .map(|k| {
            if k.test == PACKAGE_LEVEL { k.package.clone() } else { format!("{}#{}", k.package, method_name(&k.test)) }
        })
        .collect();
    filters.dedup();
    filters.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::domain::TestOutcome;
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    fn suite(build: JvmBuild, exit: i32) -> (JvmSuite, Arc<RecordingBackend>) {
        let backend = Arc::new(RecordingBackend::answering(ExecResult {
            exit_code: Some(exit),
            output_tail: "[ERROR] COMPILATION ERROR".into(),
            ..ExecResult::default()
        }));
        (JvmSuite::new(build, backend.clone()), backend)
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    const REPORT: &str = r#"<testsuite name="com.shop.CartTest">
        <testcase name="total()" classname="com.shop.CartTest"/>
        <testcase name="tax()" classname="com.shop.CartTest"><failure message="boom"/></testcase>
    </testsuite>"#;

    #[test]
    fn detection_prefers_maven_when_both_exist() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "build.gradle.kts", "");
        assert!(suite(JvmBuild::Gradle, 0).0.detect(root.path()));
        write(root.path(), "pom.xml", "<project/>");
        assert!(suite(JvmBuild::Maven, 0).0.detect(root.path()));
        assert!(!suite(JvmBuild::Gradle, 0).0.detect(root.path()));
    }

    #[tokio::test]
    async fn maven_run_parses_fresh_surefire_reports_of_every_module() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "pom.xml", "<project/>");
        write(root.path(), "mvnw", "#!/bin/sh");
        write(root.path(), "cart/target/surefire-reports/TEST-com.shop.CartTest.xml", REPORT);
        write(root.path(), "cart/target/surefire-reports/com.shop.CartTest.txt", "ignored");
        let (suite, backend) = suite(JvmBuild::Maven, 0);
        let only = [TestKey::new("com.shop.CartTest", "tax()"), TestKey::new("com.shop.Other", PACKAGE_LEVEL)];
        let run = suite.run(root.path(), &only).await.unwrap();
        assert_eq!(run.outcome("com.shop.CartTest", "tax()"), Some(TestOutcome::Fail));
        let argv = &backend.requests.lock().unwrap()[0].argv;
        assert_eq!(&argv[..2], &["sh", "mvnw"]);
        assert!(argv.contains(&"-Dtest=com.shop.CartTest#tax,com.shop.Other".to_string()));
        assert_eq!(argv.last().unwrap(), "test");
    }

    #[tokio::test]
    async fn gradle_without_reports_is_no_results_and_always_runs_the_whole_suite() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "settings.gradle", "");
        let (suite, backend) = suite(JvmBuild::Gradle, 1);
        let error = suite.run(root.path(), &[TestKey::new("A", "b()")]).await.unwrap_err();
        assert!(matches!(error, ImpactError::NoResults(o) if o.contains("COMPILATION ERROR")));
        let argv = &backend.requests.lock().unwrap()[0].argv;
        assert_eq!(argv, &strings(&["gradle", "cleanTest", "test", "--continue", "--console=plain"]));
    }

    #[tokio::test]
    async fn gradle_reads_build_test_results() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "build.gradle", "");
        write(root.path(), "app/build/test-results/test/TEST-com.shop.CartTest.xml", REPORT);
        let run = suite(JvmBuild::Gradle, 1).0.run(root.path(), &[]).await.unwrap();
        assert_eq!(run.outcome("com.shop.CartTest", "total()"), Some(TestOutcome::Pass));
    }

    #[test]
    fn reproduce_and_locate_point_at_the_test_method() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "gradlew", "#!/bin/sh");
        write(root.path(), "src/test/kotlin/com/shop/CartTest.kt", "class CartTest {\n  @Test\n  fun tax() {}\n}\n");
        let (gradle, _) = suite(JvmBuild::Gradle, 0);
        let key = TestKey::new("com.shop.CartTest", "tax()");
        assert_eq!(gradle.reproduce_command(root.path(), &key), "./gradlew test --tests com.shop.CartTest.tax");
        assert_eq!(gradle.reproduce_command(root.path(), &TestKey::new("gradle", PACKAGE_LEVEL)), "./gradlew test");
        assert_eq!(gradle.locate(root.path(), &key), Some(("src/test/kotlin/com/shop/CartTest.kt".into(), 3)));
        let (maven, _) = suite(JvmBuild::Maven, 0);
        let param = TestKey::new("com.shop.CartTest", "rate(int)[2]");
        assert_eq!(maven.reproduce_command(root.path(), &param), "mvn -Dtest=com.shop.CartTest#rate test");
        assert_eq!(maven.locate(root.path(), &TestKey::new("com.Missing", "x")), None);
    }
}
