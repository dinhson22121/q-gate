use std::cmp::Reverse;
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::join_all;

use super::config::QualityConfig;
use super::ports::{CheckInput, CheckOutcome, QualityCheck};
use crate::quality::domain::{ChangeIndex, ChangedFile, QualityFinding, Verdict};

/// Pre-existing findings kept for reference; the rest are summarised in a note.
const MAX_REFERENCE_FINDINGS: usize = 25;

/// What the caller knows about the change.
#[derive(Debug, Clone)]
pub struct QualityRequest {
    /// Directory holding the exact head commit tree (see `cli::snapshot`).
    pub snapshot: PathBuf,
    pub base_sha: Option<String>,
    pub head_sha: String,
    pub changed_files: Vec<ChangedFile>,
}

/// Findings ordered blocking first, then new, then by severity; plus notes about skipped or failed tools.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct QualityReport {
    pub findings: Vec<QualityFinding>,
    pub notes: Vec<String>,
}

impl QualityReport {
    /// Findings that should block the gate.
    pub fn blocking(&self) -> impl Iterator<Item = &QualityFinding> {
        self.findings.iter().filter(|f| f.gate_verdict() == Verdict::Block)
    }

    /// Findings introduced by the change.
    pub fn new_findings(&self) -> impl Iterator<Item = &QualityFinding> {
        self.findings.iter().filter(|f| f.is_new)
    }
}

pub struct RunQualityChecks {
    checks: Vec<Arc<dyn QualityCheck>>,
    config: QualityConfig,
}

impl RunQualityChecks {
    pub fn new(checks: Vec<Arc<dyn QualityCheck>>, config: QualityConfig) -> Self {
        Self { checks, config }
    }

    /// Runs every enabled check whose language is selected, concurrently; never fails.
    pub async fn execute(&self, request: &QualityRequest) -> QualityReport {
        let changes = ChangeIndex::new(&request.changed_files);
        let input = CheckInput { request, changes: &changes, languages: &self.config.languages };
        let mut notes = Vec::new();
        let mut runs = Vec::new();
        for check in self.checks.iter().filter(|c| self.config.tools.contains(&c.tool())) {
            if self.applies(check.as_ref()) {
                runs.push(check.run(&input));
            } else {
                notes.push(format!("{} skipped: none of its languages is selected", check.tool().as_str()));
            }
        }
        let mut findings = Vec::new();
        for outcome in join_all(runs).await {
            let CheckOutcome { findings: found, notes: outcome_notes } = outcome;
            findings.extend(found);
            notes.extend(outcome_notes);
        }
        for finding in &mut findings {
            changes.classify(finding);
        }
        let mut findings = merge_duplicates(findings);
        findings.sort_by_key(|f| {
            (f.gate_verdict() != Verdict::Block, !f.is_new, Reverse(f.severity), f.file.clone(), f.line)
        });
        cap_reference_findings(&mut findings, &mut notes);
        QualityReport { findings, notes }
    }

    fn applies(&self, check: &dyn QualityCheck) -> bool {
        let languages = check.languages();
        languages.is_empty() || languages.iter().any(|l| self.config.languages.contains(l))
    }
}

/// The same vulnerability in the same package reported by two tools: keep the first (reachability-aware
/// govulncheck sorts before osv-scanner because checks run in registration order), merging the newness.
fn merge_duplicates(findings: Vec<QualityFinding>) -> Vec<QualityFinding> {
    let mut kept: Vec<QualityFinding> = Vec::new();
    for finding in findings {
        match kept.iter_mut().find(|existing| is_same_vulnerability(existing, &finding)) {
            Some(existing) => {
                existing.is_new |= finding.is_new;
                existing.severity = existing.severity.max(finding.severity);
            }
            None => kept.push(finding),
        }
    }
    kept
}

fn is_same_vulnerability(a: &QualityFinding, b: &QualityFinding) -> bool {
    let (Some(da), Some(db)) = (&a.dependency, &b.dependency) else {
        return false;
    };
    da.name == db.name && a.all_ids().any(|id| b.all_ids().any(|other| other == id))
}

fn cap_reference_findings(findings: &mut Vec<QualityFinding>, notes: &mut Vec<String>) {
    let mut reference = 0usize;
    let mut dropped = 0usize;
    findings.retain(|f| {
        if f.is_new {
            return true;
        }
        reference += 1;
        let keep = reference <= MAX_REFERENCE_FINDINGS;
        dropped += usize::from(!keep);
        keep
    });
    if dropped > 0 {
        notes.push(format!("{dropped} more pre-existing finding(s) not shown (never block)"));
    }
}

#[cfg(test)]
mod tests {
    use futures::future::BoxFuture;

    use super::*;
    use crate::quality::domain::finding::fixtures::finding;
    use crate::quality::domain::{DependencyRef, Language, QualityCategory, QualitySeverity, QualityTool};
    use crate::quality::infrastructure::ToolchainPaths;

    struct Fake {
        tool: QualityTool,
        languages: &'static [Language],
        outcome: fn() -> CheckOutcome,
    }

    impl QualityCheck for Fake {
        fn tool(&self) -> QualityTool {
            self.tool
        }
        fn languages(&self) -> &'static [Language] {
            self.languages
        }
        fn run<'a>(&'a self, _: &'a CheckInput<'a>) -> BoxFuture<'a, CheckOutcome> {
            let outcome = (self.outcome)();
            Box::pin(async move { outcome })
        }
    }

    fn request(files: Vec<ChangedFile>) -> QualityRequest {
        QualityRequest {
            snapshot: PathBuf::from("/snap"),
            base_sha: Some("b".into()),
            head_sha: "h".into(),
            changed_files: files,
        }
    }

    fn use_case(checks: Vec<Fake>, languages: &[Language]) -> RunQualityChecks {
        let config = QualityConfig::new(languages.iter().copied(), ToolchainPaths::default());
        RunQualityChecks::new(checks.into_iter().map(|c| Arc::new(c) as Arc<dyn QualityCheck>).collect(), config)
    }

    fn secret_at(file: &str, line: u32) -> QualityFinding {
        let mut f = finding(QualityCategory::Secret, QualitySeverity::Critical, false);
        f.file = file.into();
        f.line = line;
        f
    }

    fn changed(path: &str, line: u32) -> ChangedFile {
        ChangedFile::new(path, false, [(line, "x".to_string())].into())
    }

    #[tokio::test]
    async fn new_secret_blocks_and_pre_existing_one_does_not() {
        let check = Fake {
            tool: QualityTool::Gitleaks,
            languages: &[],
            outcome: || CheckOutcome::found(vec![secret_at("old.go", 3), secret_at("new.go", 7)]),
        };
        let report =
            use_case(vec![check], &[]).execute(&request(vec![changed("new.go", 7), changed("old.go", 99)])).await;
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.findings[0].file, "new.go", "blocking findings come first");
        assert!(report.findings[0].is_blocking());
        assert!(!report.findings[1].is_new && !report.findings[1].is_blocking());
        assert_eq!(report.blocking().count(), 1);
        assert_eq!(report.new_findings().count(), 1);
    }

    #[tokio::test]
    async fn disabled_tools_do_not_run_and_unselected_languages_are_noted() {
        let go_only =
            Fake { tool: QualityTool::Govulncheck, languages: &[Language::Go], outcome: || panic!("must not run") };
        let disabled = Fake { tool: QualityTool::Depguard, languages: &[], outcome: || panic!("must not run") };
        let mut use_case = use_case(vec![go_only, disabled], &[Language::Java]);
        use_case.config.tools.remove(&QualityTool::Depguard);
        let report = use_case.execute(&request(vec![])).await;
        assert!(report.findings.is_empty());
        assert_eq!(report.notes, ["govulncheck skipped: none of its languages is selected"]);
    }

    #[tokio::test]
    async fn notes_from_checks_are_collected() {
        let check = Fake {
            tool: QualityTool::Gitleaks,
            languages: &[],
            outcome: || CheckOutcome::skipped("gitleaks is not installed"),
        };
        let report = use_case(vec![check], &[]).execute(&request(vec![])).await;
        assert_eq!(report.notes, ["gitleaks is not installed"]);
    }

    fn vuln(tool: QualityTool, id: &str, severity: QualitySeverity) -> QualityFinding {
        let mut f = finding(QualityCategory::DependencyVuln, severity, false);
        f.tool = tool;
        f.identifier = id.into();
        f.dependency = Some(DependencyRef {
            name: "m".into(),
            version: "1.2.3".into(),
            manifest: Some("go.mod".into()),
            fixed_version: None,
        });
        f
    }

    #[tokio::test]
    async fn same_vulnerability_from_two_tools_is_merged() {
        let govuln = Fake {
            tool: QualityTool::Govulncheck,
            languages: &[],
            outcome: || CheckOutcome::found(vec![vuln(QualityTool::Govulncheck, "CVE-1", QualitySeverity::High)]),
        };
        let osv = Fake {
            tool: QualityTool::OsvScanner,
            languages: &[],
            outcome: || {
                let mut f = vuln(QualityTool::OsvScanner, "GO-1", QualitySeverity::Critical);
                f.aliases = vec!["CVE-1".into()];
                CheckOutcome::found(vec![f, vuln(QualityTool::OsvScanner, "CVE-2", QualitySeverity::Low)])
            },
        };
        let files = vec![ChangedFile::new("go.mod", false, [(2, "m v1.2.3".to_string())].into())];
        let report = use_case(vec![govuln, osv], &[]).execute(&request(files)).await;
        assert_eq!(report.findings.len(), 2);
        let merged = &report.findings[0];
        assert_eq!(
            (merged.tool, merged.severity, merged.is_new),
            (QualityTool::Govulncheck, QualitySeverity::Critical, true)
        );
        assert!(merged.is_blocking());
    }

    #[tokio::test]
    async fn pre_existing_findings_are_capped_with_a_note() {
        let check = Fake {
            tool: QualityTool::Gitleaks,
            languages: &[],
            outcome: || CheckOutcome::found((0..30).map(|i| secret_at("old.go", i + 1)).collect()),
        };
        let report = use_case(vec![check], &[]).execute(&request(vec![])).await;
        assert_eq!(report.findings.len(), MAX_REFERENCE_FINDINGS);
        assert_eq!(report.notes, ["5 more pre-existing finding(s) not shown (never block)"]);
    }
}
