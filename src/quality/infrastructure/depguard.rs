use std::path::{Path, PathBuf};

use futures::future::BoxFuture;

use super::runner::{ToolRunner, path_arg, unreadable_note};
use crate::quality::application::{CheckInput, CheckOutcome, QualityCheck};
use crate::quality::domain::parsers::golangci;
use crate::quality::domain::{ImportBoundary, Language, QualityTool};

const GO_MOD: &str = "go.mod";
const CONFIG_FILE: &str = ".qgate-depguard.yml";
const MODULE_PLACEHOLDER: &str = "{module}";

pub struct DepguardCheck {
    runner: ToolRunner,
    binary: Option<PathBuf>,
    boundaries: Vec<ImportBoundary>,
}

impl DepguardCheck {
    pub fn new(runner: ToolRunner, binary: Option<PathBuf>, boundaries: Vec<ImportBoundary>) -> Self {
        Self { runner, binary, boundaries }
    }

    async fn check(&self, input: &CheckInput<'_>) -> CheckOutcome {
        let snapshot = input.snapshot();
        let go_mod = std::fs::read_to_string(snapshot.join(GO_MOD));
        if self.boundaries.is_empty() || !input.changes.paths().any(|p| p.ends_with(".go")) || go_mod.is_err() {
            return CheckOutcome::default();
        }
        let Some(binary) = &self.binary else {
            return CheckOutcome::skipped(
                "golangci-lint is not installed: Go import boundaries not checked (add `go` to QGATE_TOOLCHAINS)",
            );
        };
        let module = go_mod.ok().and_then(|text| module_path(&text));
        let config_path = snapshot.join(CONFIG_FILE);
        if let Err(error) = tokio::fs::write(&config_path, render_config(&self.boundaries, module.as_deref())).await {
            return CheckOutcome::skipped(format!("depguard config could not be written: {error}"));
        }
        let outcome = self.run_tool(binary, &config_path, snapshot).await;
        let _ = tokio::fs::remove_file(&config_path).await;
        outcome
    }

    async fn run_tool(&self, binary: &Path, config: &Path, snapshot: &Path) -> CheckOutcome {
        let argv = [path_arg(binary), "run".into(), "--config".into(), path_arg(config)]
            .into_iter()
            .chain(
                [
                    "--output.json.path=stdout",
                    "--output.text.path=",
                    "--show-stats=false",
                    "--issues-exit-code=0",
                    "./...",
                ]
                .map(String::from),
            )
            .collect();
        let result = match self.runner.run("golangci-lint", argv, snapshot).await {
            Ok(result) => result,
            Err(note) => return CheckOutcome::skipped(note),
        };
        match golangci::parse(&result.output_tail, snapshot, &self.boundaries) {
            Some(findings) => CheckOutcome::found(findings),
            None => CheckOutcome::skipped(unreadable_note("golangci-lint", &result)),
        }
    }
}

impl QualityCheck for DepguardCheck {
    fn tool(&self) -> QualityTool {
        QualityTool::Depguard
    }

    fn languages(&self) -> &'static [Language] {
        &[Language::Go]
    }

    fn run<'a>(&'a self, input: &'a CheckInput<'a>) -> BoxFuture<'a, CheckOutcome> {
        Box::pin(self.check(input))
    }
}

fn module_path(go_mod: &str) -> Option<String> {
    go_mod.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("module")?;
        let path = rest.strip_prefix(char::is_whitespace)?.trim();
        (!path.is_empty()).then(|| path.trim_matches('"').to_string())
    })
}

fn render_config(boundaries: &[ImportBoundary], module: Option<&str>) -> String {
    let quote = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string());
    let mut yaml = String::from(
        "version: \"2\"\nrun:\n  tests: false\nlinters:\n  default: none\n  enable:\n    - depguard\n  settings:\n    depguard:\n      rules:\n",
    );
    for boundary in boundaries {
        let denied: Vec<String> = boundary.deny.iter().filter_map(|pkg| resolve(pkg, module)).collect();
        if denied.is_empty() {
            continue;
        }
        yaml.push_str(&format!("        {}:\n          files:\n", boundary.id));
        for glob in &boundary.scope_globs {
            yaml.push_str(&format!("            - {}\n", quote(&absolute_glob(glob))));
        }
        yaml.push_str("          deny:\n");
        for pkg in denied {
            yaml.push_str(&format!(
                "            - pkg: {}\n              desc: {}\n",
                quote(&pkg),
                quote(&boundary.title)
            ));
        }
    }
    yaml.push_str("issues:\n  max-issues-per-linter: 0\n  max-same-issues: 0\n");
    yaml
}

fn absolute_glob(glob: &str) -> String {
    if glob.starts_with("**") || glob.starts_with('/') { glob.to_string() } else { format!("**/{glob}") }
}

fn resolve(pkg: &str, module: Option<&str>) -> Option<String> {
    if !pkg.contains(MODULE_PLACEHOLDER) {
        return Some(pkg.to_string());
    }
    module.map(|m| pkg.replace(MODULE_PLACEHOLDER, m))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::quality::application::QualityRequest;
    use crate::quality::domain::{ChangeIndex, ChangedFile};
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    const REPORT: &str = include_str!("../fixtures/golangci.json");

    fn boundary() -> ImportBoundary {
        ImportBoundary {
            id: "domain-purity".into(),
            title: "Pure \"domain\"".into(),
            scope_globs: vec!["internal/domain/**".into()],
            deny: vec!["{module}/internal/infra".into(), "database/sql".into()],
            approved: true,
        }
    }

    #[test]
    fn config_has_only_depguard_and_resolves_the_module() {
        let yaml = render_config(&[boundary()], Some("example.com/app"));
        assert!(yaml.contains("default: none"));
        assert!(yaml.contains("        domain-purity:\n          files:\n            - \"**/internal/domain/**\"\n"));
        assert!(
            yaml.contains("- pkg: \"example.com/app/internal/infra\"\n              desc: \"Pure \\\"domain\\\"\"")
        );
        assert!(yaml.contains("- pkg: \"database/sql\""));
    }

    #[test]
    fn unresolvable_module_placeholder_drops_that_entry_and_empty_rules() {
        let yaml = render_config(&[boundary()], None);
        assert!(!yaml.contains("{module}") && yaml.contains("database/sql"));
        let mut only_module = boundary();
        only_module.deny = vec!["{module}/x".into()];
        assert!(!render_config(&[only_module], None).contains("domain-purity"));
        assert_eq!(absolute_glob("/abs/**"), "/abs/**");
        assert_eq!(absolute_glob("**/d/**"), "**/d/**");
    }

    #[test]
    fn module_path_reads_the_module_line() {
        assert_eq!(module_path("module example.com/app\n\ngo 1.22\n").as_deref(), Some("example.com/app"));
        assert_eq!(module_path("module \"x/y\"\n").as_deref(), Some("x/y"));
        assert_eq!(module_path("go 1.22\nmodules-are-great\n"), None);
        assert_eq!(module_path("module\n"), None);
    }

    fn run(
        binary: Option<&str>,
        boundaries: Vec<ImportBoundary>,
        with_go_mod: bool,
        changed: &[&str],
    ) -> (CheckOutcome, Vec<String>, bool) {
        let snapshot = tempfile::tempdir().unwrap();
        if with_go_mod {
            std::fs::write(snapshot.path().join("go.mod"), "module example.com/app\n").unwrap();
        }
        let backend = Arc::new(RecordingBackend::answering(ExecResult {
            exit_code: Some(0),
            output_tail: REPORT.into(),
            ..Default::default()
        }));
        let check = DepguardCheck::new(ToolRunner::new(backend.clone(), 60), binary.map(PathBuf::from), boundaries);
        let files = changed.iter().map(|p| ChangedFile::new(*p, true, Default::default())).collect();
        let request = QualityRequest {
            snapshot: snapshot.path().into(),
            base_sha: None,
            head_sha: "h".into(),
            changed_files: files,
        };
        let changes = ChangeIndex::new(&request.changed_files);
        let languages = Default::default();
        let input = CheckInput { request: &request, changes: &changes, languages: &languages };
        let outcome = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(check.check(&input));
        let argv = backend.requests.lock().unwrap().first().map(|r| r.argv.clone()).unwrap_or_default();
        (outcome, argv, snapshot.path().join(CONFIG_FILE).exists())
    }

    #[test]
    fn runs_golangci_lint_with_the_generated_config_and_cleans_up() {
        let (outcome, argv, config_left) =
            run(Some("/tc/golangci-lint"), vec![boundary()], true, &["internal/domain/user.go"]);
        assert_eq!(outcome.findings.len(), 1);
        assert_eq!(outcome.findings[0].identifier, "domain-purity");
        assert_eq!(&argv[..3], ["/tc/golangci-lint", "run", "--config"]);
        assert!(argv.contains(&"--output.json.path=stdout".to_string()));
        assert!(!config_left, "the generated config is removed");
    }

    #[test]
    fn skips_quietly_when_there_is_nothing_to_check() {
        for (boundaries, go_mod, changed) in
            [(vec![], true, "a.go"), (vec![boundary()], false, "a.go"), (vec![boundary()], true, "README.md")]
        {
            let (outcome, argv, _) = run(Some("/tc/golangci-lint"), boundaries, go_mod, &[changed]);
            assert_eq!(outcome, CheckOutcome::default());
            assert!(argv.is_empty());
        }
    }

    #[test]
    fn missing_binary_is_a_note() {
        let (outcome, argv, _) = run(None, vec![boundary()], true, &["a.go"]);
        assert!(outcome.notes[0].starts_with("golangci-lint is not installed"));
        assert!(argv.is_empty());
    }
}
