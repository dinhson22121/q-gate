//! Language profiles: where a module starts, where a generated test may be written and the exact
//! command that runs that ONE test. Nothing here is chosen by the model; it can only pick one of
//! the [`TestTarget`]s computed from the changed files.

use super::language::TestLanguage;
use super::paths::{join, parent_dir, strip_dir};

/// Read-only probes of the snapshot that the profiles need (implemented by the workspace adapter).
pub trait ProbeFs {
    /// True when the snapshot-relative path exists inside the snapshot.
    fn has_path(&self, rel: &str) -> bool;
    /// Names of the entries of a snapshot-relative directory (empty when missing).
    fn entries(&self, rel: &str) -> Vec<String>;
}

/// One allowed place for the scenario's test, with the command that runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestTarget {
    pub language: TestLanguage,
    /// Module root (go.mod / pom.xml / build.gradle / pyproject.toml / Cargo.toml dir); "" = snapshot root.
    pub module_dir: String,
    /// Snapshot-relative path of the new test file.
    pub test_file: String,
    /// Program and arguments, run in `module_dir` without a shell.
    pub command: Vec<String>,
}

/// Name parts derived from a scenario id ("S1" -> `s1` / `S1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioName {
    pub snake: String,
    pub pascal: String,
}

impl ScenarioName {
    /// Keeps ASCII letters and digits only, so the id can never inject path or regex syntax.
    pub fn from_id(id: &str) -> Self {
        let clean: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
        let clean = if clean.is_empty() { "scenario".to_string() } else { clean };
        let mut chars = clean.chars();
        let pascal = chars.next().map(|c| c.to_ascii_uppercase()).into_iter().chain(chars).collect();
        Self { snake: clean.to_ascii_lowercase(), pascal }
    }
}

const GO_MARKERS: &[&str] = &["go.mod"];
const MAVEN_MARKER: &str = "pom.xml";
const GRADLE_MARKERS: &[&str] = &["build.gradle", "build.gradle.kts"];
const JAVA_MARKERS: &[&str] = &["pom.xml", "build.gradle", "build.gradle.kts"];
const PYTHON_MARKERS: &[&str] = &["pyproject.toml", "setup.cfg", "pytest.ini"];
const RUST_MARKERS: &[&str] = &["Cargo.toml"];
const JAVA_SOURCE_ROOTS: &[&str] = &["src/main/java/", "src/test/java/"];

/// Every allowed target for the scenario, one per distinct location, in change order.
pub fn test_targets(
    language: TestLanguage,
    changed: &[String],
    name: &ScenarioName,
    fs: &impl ProbeFs,
) -> Vec<TestTarget> {
    let mut targets: Vec<TestTarget> = Vec::new();
    for file in changed {
        for target in targets_for_file(language, file, name, fs) {
            if !targets.iter().any(|t| t.test_file == target.test_file) {
                targets.push(target);
            }
        }
    }
    targets
}

fn targets_for_file(language: TestLanguage, file: &str, name: &ScenarioName, fs: &impl ProbeFs) -> Vec<TestTarget> {
    let markers = match language {
        TestLanguage::Go => GO_MARKERS,
        TestLanguage::Java => JAVA_MARKERS,
        TestLanguage::Python => PYTHON_MARKERS,
        TestLanguage::Rust => RUST_MARKERS,
    };
    let Some(module_dir) = find_module(fs, parent_dir(file), markers) else {
        return Vec::new();
    };
    match language {
        TestLanguage::Go => vec![go_target(&module_dir, file, name)],
        TestLanguage::Java => java_target(&module_dir, file, name, fs).into_iter().collect(),
        TestLanguage::Python => python_targets(&module_dir, file, name, fs),
        TestLanguage::Rust => vec![rust_target(&module_dir, name)],
    }
}

/// Nearest directory at or above `start` containing one of `markers`.
fn find_module(fs: &impl ProbeFs, start: &str, markers: &[&str]) -> Option<String> {
    let mut dir = start.to_string();
    loop {
        if markers.iter().any(|marker| fs.has_path(&join(&dir, marker))) {
            return Some(dir);
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent_dir(&dir).to_string();
    }
}

fn go_target(module_dir: &str, file: &str, name: &ScenarioName) -> TestTarget {
    let package_dir = parent_dir(file);
    let package = match strip_dir(package_dir, module_dir) {
        Some("") | None => ".".to_string(),
        Some(rel) => format!("./{rel}"),
    };
    TestTarget {
        language: TestLanguage::Go,
        module_dir: module_dir.to_string(),
        test_file: join(package_dir, &format!("qgate_{}_test.go", name.snake)),
        command: strings(&["go", "test", &package, "-run", &format!("^TestQgate{}", name.pascal), "-count=1"]),
    }
}

fn java_target(module_dir: &str, file: &str, name: &ScenarioName, fs: &impl ProbeFs) -> Option<TestTarget> {
    let in_module = strip_dir(file, module_dir)?;
    let package_path = JAVA_SOURCE_ROOTS.iter().find_map(|root| in_module.strip_prefix(root))?;
    let class = format!("Qgate{}Test", name.pascal);
    let test_dir = join(&join(module_dir, "src/test/java"), parent_dir(package_path));
    let command = if fs.has_path(&join(module_dir, MAVEN_MARKER)) {
        let program = wrapper(fs, module_dir, "mvnw").unwrap_or_else(|| "mvn".into());
        vec![
            program,
            "-q".into(),
            format!("-Dtest={class}"),
            "-Dsurefire.failIfNoSpecifiedTests=false".into(),
            "test".into(),
        ]
    } else if GRADLE_MARKERS.iter().any(|m| fs.has_path(&join(module_dir, m))) {
        let program = wrapper(fs, module_dir, "gradlew").unwrap_or_else(|| "gradle".into());
        vec![program, "test".into(), "--tests".into(), class.clone()]
    } else {
        return None;
    };
    Some(TestTarget {
        language: TestLanguage::Java,
        module_dir: module_dir.to_string(),
        test_file: join(&test_dir, &format!("{class}.java")),
        command,
    })
}

/// The build wrapper in `module_dir` or an ancestor, as a path relative to `module_dir`.
fn wrapper(fs: &impl ProbeFs, module_dir: &str, script: &str) -> Option<String> {
    let mut dir = module_dir.to_string();
    let mut ups = 0;
    loop {
        if fs.has_path(&join(&dir, script)) {
            return Some(if ups == 0 { format!("./{script}") } else { format!("{}{script}", "../".repeat(ups)) });
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent_dir(&dir).to_string();
        ups += 1;
    }
}

/// `<module>/tests/`, plus directories that already hold pytest files (next to existing tests).
fn python_targets(module_dir: &str, file: &str, name: &ScenarioName, fs: &impl ProbeFs) -> Vec<TestTarget> {
    let file_dir = parent_dir(file);
    let mut dirs = vec![join(module_dir, "tests")];
    for candidate in [join(module_dir, "test"), join(file_dir, "tests"), file_dir.to_string()] {
        let has_pytest_files = fs.entries(&candidate).iter().any(|e| e.starts_with("test_") && e.ends_with(".py"));
        if has_pytest_files && !dirs.contains(&candidate) {
            dirs.push(candidate);
        }
    }
    dirs.into_iter()
        .map(|dir| {
            let test_file = join(&dir, &format!("test_qgate_{}.py", name.snake));
            let in_module = strip_dir(&test_file, module_dir).unwrap_or(&test_file).to_string();
            TestTarget {
                language: TestLanguage::Python,
                module_dir: module_dir.to_string(),
                command: strings(&["python", "-m", "pytest", &in_module, "-q"]),
                test_file,
            }
        })
        .collect()
}

fn rust_target(module_dir: &str, name: &ScenarioName) -> TestTarget {
    let test_name = format!("qgate_{}", name.snake);
    TestTarget {
        language: TestLanguage::Rust,
        module_dir: module_dir.to_string(),
        test_file: join(&join(module_dir, "tests"), &format!("{test_name}.rs")),
        command: strings(&["cargo", "test", "--test", &test_name, "-q"]),
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::BTreeSet;

    use super::ProbeFs;

    /// In-memory snapshot: a set of file paths.
    pub struct FakeFs(pub BTreeSet<String>);

    impl FakeFs {
        pub fn of(files: &[&str]) -> Self {
            Self(files.iter().map(|s| s.to_string()).collect())
        }
    }

    impl ProbeFs for FakeFs {
        fn has_path(&self, rel: &str) -> bool {
            self.0.contains(rel)
        }

        fn entries(&self, rel: &str) -> Vec<String> {
            let prefix = if rel.is_empty() { String::new() } else { format!("{rel}/") };
            self.0
                .iter()
                .filter_map(|f| f.strip_prefix(&prefix))
                .filter(|rest| !rest.contains('/'))
                .map(str::to_string)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::FakeFs;
    use super::*;

    fn name() -> ScenarioName {
        ScenarioName::from_id("S1")
    }

    fn targets(language: TestLanguage, changed: &[&str], fs: &FakeFs) -> Vec<TestTarget> {
        let changed: Vec<String> = changed.iter().map(|s| s.to_string()).collect();
        test_targets(language, &changed, &name(), fs)
    }

    #[test]
    fn scenario_names_are_sanitized() {
        assert_eq!(ScenarioName::from_id("S1"), ScenarioName { snake: "s1".into(), pascal: "S1".into() });
        assert_eq!(ScenarioName::from_id("s-2/../x"), ScenarioName { snake: "s2x".into(), pascal: "S2x".into() });
        assert_eq!(ScenarioName::from_id("--").snake, "scenario");
    }

    #[test]
    fn go_test_lives_in_the_package_dir() {
        let fs = FakeFs::of(&["svc/go.mod", "svc/pay/pay.go", "svc/main.go"]);
        let found = targets(TestLanguage::Go, &["svc/pay/pay.go", "svc/main.go", "svc/pay/pay.go"], &fs);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].module_dir, "svc");
        assert_eq!(found[0].test_file, "svc/pay/qgate_s1_test.go");
        assert_eq!(found[0].command, ["go", "test", "./pay", "-run", "^TestQgateS1", "-count=1"]);
        assert_eq!(found[1].test_file, "svc/qgate_s1_test.go");
        assert_eq!(found[1].command[2], ".");
    }

    #[test]
    fn no_module_marker_means_no_target() {
        let fs = FakeFs::of(&["pay/pay.go"]);
        assert!(targets(TestLanguage::Go, &["pay/pay.go"], &fs).is_empty());
    }

    #[test]
    fn java_maven_mirrors_the_package_under_src_test_java() {
        let fs = FakeFs::of(&["pom.xml", "api/pom.xml", "mvnw", "api/src/main/java/com/shop/Cart.java"]);
        let found = targets(TestLanguage::Java, &["api/src/main/java/com/shop/Cart.java"], &fs);
        assert_eq!(found[0].test_file, "api/src/test/java/com/shop/QgateS1Test.java");
        assert_eq!(found[0].module_dir, "api");
        assert_eq!(
            found[0].command,
            ["../mvnw", "-q", "-Dtest=QgateS1Test", "-Dsurefire.failIfNoSpecifiedTests=false", "test"]
        );
        let no_wrapper = FakeFs::of(&["pom.xml"]);
        let found = targets(TestLanguage::Java, &["src/main/java/Cart.java"], &no_wrapper);
        assert_eq!(found[0].command[0], "mvn");
        assert_eq!(found[0].test_file, "src/test/java/QgateS1Test.java");
    }

    #[test]
    fn java_gradle_uses_the_wrapper_when_present() {
        let fs = FakeFs::of(&["build.gradle.kts", "gradlew"]);
        let found = targets(TestLanguage::Java, &["src/test/java/a/CartTest.java"], &fs);
        assert_eq!(found[0].command, ["./gradlew", "test", "--tests", "QgateS1Test"]);
        assert_eq!(found[0].test_file, "src/test/java/a/QgateS1Test.java");
        let fs = FakeFs::of(&["build.gradle"]);
        assert_eq!(targets(TestLanguage::Java, &["src/main/java/A.java"], &fs)[0].command[0], "gradle");
    }

    #[test]
    fn java_outside_a_source_root_has_no_target() {
        let fs = FakeFs::of(&["pom.xml"]);
        assert!(targets(TestLanguage::Java, &["tools/Gen.java"], &fs).is_empty());
        let fs = FakeFs::of(&["a/b/pom.xml"]);
        assert!(java_target("x", "a/b/src/main/java/A.java", &name(), &fs).is_none());
    }

    #[test]
    fn python_tests_dir_and_existing_test_dirs() {
        let fs = FakeFs::of(&["pyproject.toml", "app/cart.py", "app/test_cart.py", "test/test_old.py"]);
        let found = targets(TestLanguage::Python, &["app/cart.py"], &fs);
        let files: Vec<_> = found.iter().map(|t| t.test_file.as_str()).collect();
        assert_eq!(files, ["tests/test_qgate_s1.py", "test/test_qgate_s1.py", "app/test_qgate_s1.py"]);
        assert_eq!(found[0].command, ["python", "-m", "pytest", "tests/test_qgate_s1.py", "-q"]);
    }

    #[test]
    fn python_command_is_relative_to_the_module() {
        let fs = FakeFs::of(&["svc/setup.cfg"]);
        let found = targets(TestLanguage::Python, &["svc/x.py"], &fs);
        assert_eq!(found[0].test_file, "svc/tests/test_qgate_s1.py");
        assert_eq!(found[0].command[3], "tests/test_qgate_s1.py");
    }

    #[test]
    fn rust_integration_test_in_the_crate() {
        let fs = FakeFs::of(&["Cargo.toml", "crates/core/Cargo.toml"]);
        let found = targets(TestLanguage::Rust, &["crates/core/src/lib.rs", "src/main.rs"], &fs);
        assert_eq!(found[0].test_file, "crates/core/tests/qgate_s1.rs");
        assert_eq!(found[0].command, ["cargo", "test", "--test", "qgate_s1", "-q"]);
        assert_eq!(found[1].test_file, "tests/qgate_s1.rs");
    }
}
