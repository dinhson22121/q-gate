//! Runs the deterministic quality tools and regression detection for one reviewed commit and turns their results
//! into review findings. Best effort by contract: a missing toolchain, a non-Go repository or a runner error
//! becomes a note, never a failed review.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use super::tool_findings::{quality_finding_to_finding, regression_to_finding};
use crate::conventions::application::{Rule, RuleStatus, Rulebook};
use crate::impact::application::{DEFAULT_MAX_PACKAGES, DetectRegressions, DetectRequest};
use crate::impact::infrastructure::GoCli;
use crate::quality::domain::parse_boundary_directives;
use crate::quality::{ChangedFile, ImportBoundary, Language, QualityConfig, QualityRequest, ToolchainPaths};
use crate::review::domain::agent_results::AgentReport;
use crate::review::domain::change_set::{ChangeSet, ChangeStatus, DiffLine};
use crate::review::domain::finding::Finding;
use crate::review::domain::gate::{GateDecision, GateInput, GatePolicy};
use crate::review::domain::report::ReviewReport;
use crate::sandbox::application::ExecBackend;
use crate::settings::application::TestLanguage as SettingsLanguage;

/// Where the container image installs the quality tools (`docker/toolchains/*.sh`).
pub const TOOLCHAINS_DIR: &str = "/toolchains";

/// Everything one run needs; snapshots are exported by the caller and outlive the run.
pub struct ToolChecks<'a> {
    pub exec: Arc<dyn ExecBackend>,
    pub head_snapshot: &'a Path,
    /// Exported base commit; `None` skips regression detection (root commit, export failed).
    pub base_snapshot: Option<&'a Path>,
    pub base_sha: Option<String>,
    pub head_sha: String,
    pub changes: ChangeSet,
    pub languages: Vec<Language>,
    pub boundaries: Vec<ImportBoundary>,
    pub toolchains: ToolchainPaths,
    pub quality_enabled: bool,
    pub regression_enabled: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ToolOutcome {
    pub findings: Vec<Finding>,
    pub notes: Vec<String>,
}

impl ToolChecks<'_> {
    pub async fn run(&self) -> ToolOutcome {
        let (changed, paths) = changed_files(&self.changes);
        let mut outcome = ToolOutcome::default();
        if self.quality_enabled {
            self.quality(changed, &mut outcome).await;
        }
        if self.regression_enabled {
            self.regression(paths, &mut outcome).await;
        }
        outcome
    }

    async fn quality(&self, changed_files: Vec<ChangedFile>, outcome: &mut ToolOutcome) {
        let config = QualityConfig::new(self.languages.iter().copied(), self.toolchains.clone())
            .with_boundaries(self.boundaries.clone());
        let request = QualityRequest {
            snapshot: self.head_snapshot.to_path_buf(),
            base_sha: self.base_sha.clone(),
            head_sha: self.head_sha.clone(),
            changed_files,
        };
        let report = crate::quality::build(self.exec.clone(), config).execute(&request).await;
        outcome.findings.extend(report.findings.iter().map(quality_finding_to_finding));
        outcome.notes.extend(report.notes);
    }

    async fn regression(&self, changed_files: Vec<String>, outcome: &mut ToolOutcome) {
        if !self.languages.contains(&Language::Go) {
            outcome.notes.push("regression check skipped: Go is not among the selected languages".into());
            return;
        }
        let (Some(base_sha), Some(base_snapshot)) = (&self.base_sha, self.base_snapshot) else {
            outcome.notes.push("regression check skipped: there is no base commit to compare with".into());
            return;
        };
        let request = DetectRequest {
            base_sha: base_sha.clone(),
            head_sha: self.head_sha.clone(),
            base_snapshot: base_snapshot.to_path_buf(),
            head_snapshot: self.head_snapshot.to_path_buf(),
            changed_files,
            max_packages: DEFAULT_MAX_PACKAGES,
        };
        let report = DetectRegressions::new(Arc::new(GoCli::new(self.exec.clone()))).execute(&request).await;
        outcome.findings.extend(report.regressions.iter().map(regression_to_finding));
        outcome.notes.extend(report.notes);
    }
}

/// The change as the quality tools see it (surviving files with their added lines) and the repo-relative paths
/// impact analysis needs (deleted files included: removing code can break its dependants).
pub fn changed_files(changes: &ChangeSet) -> (Vec<ChangedFile>, Vec<String>) {
    let paths = changes.files.iter().map(|f| f.path.clone()).collect();
    let changed = changes
        .files
        .iter()
        .filter(|f| f.status != ChangeStatus::Deleted && !f.is_binary)
        .map(|f| {
            let added: BTreeMap<u32, String> = f
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter_map(|line| match line {
                    DiffLine::Added { new_line, text } => Some((*new_line, text.clone())),
                    _ => None,
                })
                .collect();
            ChangedFile::new(f.path.clone(), f.status == ChangeStatus::Added, added)
        })
        .collect();
    (changed, paths)
}

/// Import boundaries stated by `qgate:go-import-deny` directives of the usable Rulebook rules. Approved rules block;
/// proposed ones (when inferred rules are in use) only warn.
pub fn boundaries_from_rulebook(rulebook: &Rulebook, use_inferred_rules: bool) -> Vec<ImportBoundary> {
    rulebook.rules.iter().filter(|rule| rule.is_usable(use_inferred_rules)).flat_map(rule_boundaries).collect()
}

fn rule_boundaries(rule: &Rule) -> Vec<ImportBoundary> {
    let mut texts = vec![rule.description.as_str()];
    texts.extend(rule.lint_suggestion.as_deref());
    parse_boundary_directives(&rule.id, &rule.title, rule.status == RuleStatus::Approved, &texts)
}

/// Languages the quality tools support among the configured ones.
pub fn quality_languages(configured: &[SettingsLanguage]) -> Vec<Language> {
    let mut languages: Vec<Language> = configured
        .iter()
        .filter_map(|l| match l {
            SettingsLanguage::Go => Some(Language::Go),
            SettingsLanguage::Java => Some(Language::Java),
            SettingsLanguage::Python | SettingsLanguage::Rust => None,
        })
        .collect();
    languages.dedup();
    languages
}

pub fn discover_toolchains() -> ToolchainPaths {
    ToolchainPaths::discover(Path::new(TOOLCHAINS_DIR), std::env::var("PATH").ok().as_deref())
}

/// Folds the tool findings into the report and decides the gate again so they count. `incomplete` and every
/// other gate input are the ones the review already used.
pub fn fold_into_report(report: &mut ReviewReport, outcome: ToolOutcome, policy: &GatePolicy) {
    if let Some(agents) = report.agents.as_mut() {
        agents.notes.extend(outcome.notes.iter().cloned());
    }
    for note in &outcome.notes {
        eprintln!("qgate: {note}");
    }
    report.findings.extend(outcome.findings);
    report.gate = decide(report, policy);
}

pub fn decide(report: &ReviewReport, policy: &GatePolicy) -> GateDecision {
    let agents: Option<&AgentReport> = report.agents.as_ref();
    policy.decide(GateInput {
        findings: &report.findings,
        requirements: report.requirements.as_ref(),
        checks: &report.checks,
        incomplete: report.gate.incomplete,
        agents,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::conventions::application::RuleKind;
    use crate::review::domain::change_set::{FileChange, Hunk};
    use crate::review::domain::finding::{Category, Confidence, FindingSource, Severity, fixtures::finding};
    use crate::sandbox::application::ExecResult;
    use crate::sandbox::application::fakes::RecordingBackend;

    fn file(path: &str, status: ChangeStatus, lines: Vec<DiffLine>) -> FileChange {
        FileChange {
            path: path.into(),
            status,
            is_binary: false,
            hunks: vec![Hunk { header: "@@".into(), new_start: 1, lines }],
            new_content: None,
        }
    }

    fn added(new_line: u32, text: &str) -> DiffLine {
        DiffLine::Added { new_line, text: text.into() }
    }

    fn sample_changes() -> ChangeSet {
        let mut binary = file("logo.png", ChangeStatus::Added, vec![]);
        binary.is_binary = true;
        ChangeSet {
            files: vec![
                file("a.go", ChangeStatus::Modified, vec![added(3, "x := 1"), DiffLine::Removed { text: "y".into() }]),
                file("new.go", ChangeStatus::Added, vec![added(1, "package n")]),
                file("gone.go", ChangeStatus::Deleted, vec![DiffLine::Removed { text: "z".into() }]),
                binary,
            ],
        }
    }

    #[test]
    fn changed_files_keep_added_lines_and_drop_deleted_and_binary_for_quality() {
        let (changed, paths) = changed_files(&sample_changes());
        assert_eq!(paths, vec!["a.go", "new.go", "gone.go", "logo.png"], "impact sees every path");
        let names: Vec<_> = changed.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(names, vec!["a.go", "new.go"]);
        assert_eq!(changed[0].added.get(&3).map(String::as_str), Some("x := 1"));
        assert!(!changed[0].is_new_file && changed[1].is_new_file);
    }

    fn rule(id: &str, status: RuleStatus, description: &str, lint: Option<&str>) -> Rule {
        Rule {
            id: id.into(),
            title: format!("rule {id}"),
            description: description.into(),
            kind: RuleKind::Explicit,
            status,
            scope_globs: vec![],
            examples: vec![],
            lint_suggestion: lint.map(String::from),
        }
    }

    #[test]
    fn boundaries_come_from_usable_rules_with_directives() {
        let directive = "qgate:go-import-deny scope=internal/domain/** deny=net/http";
        let rulebook = Rulebook {
            repo: "r".into(),
            fingerprint: "f".into(),
            created_at: Utc::now(),
            rules: vec![
                rule("R1", RuleStatus::Approved, directive, None),
                rule("R2", RuleStatus::Proposed, "plain prose", Some(directive)),
                rule("R3", RuleStatus::Rejected, directive, None),
            ],
        };
        let strict = boundaries_from_rulebook(&rulebook, false);
        assert_eq!(strict.len(), 1);
        assert!((strict[0].id.as_str(), strict[0].approved) == ("R1", true));
        let with_inferred = boundaries_from_rulebook(&rulebook, true);
        assert_eq!(
            with_inferred.iter().map(|b| (b.id.as_str(), b.approved)).collect::<Vec<_>>(),
            [("R1", true), ("R2", false)]
        );
    }

    #[test]
    fn only_languages_the_tools_support_are_selected() {
        let configured =
            [SettingsLanguage::Go, SettingsLanguage::Python, SettingsLanguage::Java, SettingsLanguage::Rust];
        assert_eq!(quality_languages(&configured), vec![Language::Go, Language::Java]);
        assert!(quality_languages(&[]).is_empty());
    }

    fn checks<'a>(dir: &'a Path, base: Option<&'a Path>) -> ToolChecks<'a> {
        ToolChecks {
            exec: Arc::new(RecordingBackend::answering(ExecResult::default())),
            head_snapshot: dir,
            base_snapshot: base,
            base_sha: base.map(|_| "b".repeat(40)),
            head_sha: "h".repeat(40),
            changes: sample_changes(),
            languages: vec![Language::Go],
            boundaries: Vec::new(),
            toolchains: ToolchainPaths::default(),
            quality_enabled: true,
            regression_enabled: true,
        }
    }

    #[tokio::test]
    async fn missing_toolchains_and_non_go_snapshots_become_notes_not_findings() {
        let (head, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let outcome = checks(head.path(), Some(base.path())).run().await;
        assert!(outcome.findings.is_empty());
        assert!(outcome.notes.iter().any(|n| n.contains("not installed")), "{:?}", outcome.notes);
        assert!(outcome.notes.iter().any(|n| n.contains("not a Go module")), "{:?}", outcome.notes);
    }

    #[tokio::test]
    async fn regression_is_skipped_without_a_base_or_without_go_and_quality_can_be_disabled() {
        let head = tempfile::tempdir().unwrap();
        let outcome = checks(head.path(), None).run().await;
        assert!(outcome.notes.iter().any(|n| n.contains("no base commit")), "{:?}", outcome.notes);
        let mut java_only = checks(head.path(), None);
        java_only.languages = vec![Language::Java];
        java_only.quality_enabled = false;
        let outcome = java_only.run().await;
        assert_eq!(outcome.notes, vec!["regression check skipped: Go is not among the selected languages"]);
        let mut off = checks(head.path(), None);
        (off.quality_enabled, off.regression_enabled) = (false, false);
        assert_eq!(off.run().await, ToolOutcome::default());
    }

    fn report_with(findings: Vec<Finding>) -> ReviewReport {
        let mut report = ReviewReport { findings, ..ReviewReport::default() };
        report.gate = decide(&report, &GatePolicy::default());
        report
    }

    #[test]
    fn folding_nothing_leaves_the_gate_unchanged() {
        let policy = GatePolicy::default();
        for findings in
            [vec![], vec![finding("a.go", 1, Severity::Critical, "x")], vec![finding("a.go", 1, Severity::Minor, "x")]]
        {
            let mut report = report_with(findings);
            report.gate.incomplete = true;
            report.gate = decide(&report, &policy);
            let before = report.gate.clone();
            fold_into_report(&mut report, ToolOutcome::default(), &policy);
            assert_eq!(report.gate, before);
        }
    }

    #[test]
    fn folded_blocking_findings_fail_the_gate_and_warnings_do_not() {
        let policy = GatePolicy::default();
        let mut report = report_with(vec![]);
        assert!(report.gate.passed);
        let warning = Finding { severity: Severity::Minor, ..finding("go.mod", 1, Severity::Minor, "e") };
        let blocker = Finding {
            category: Category::Architecture,
            confidence: Confidence::High,
            source: FindingSource::StaticTool("depguard".into()),
            ..finding("a.go", 2, Severity::Critical, "e")
        };
        let notes = vec!["osv-scanner not installed".to_string()];
        fold_into_report(&mut report, ToolOutcome { findings: vec![warning, blocker], notes: notes.clone() }, &policy);
        assert!(!report.gate.passed);
        assert_eq!(report.gate.blocking, vec![1]);
        assert_eq!(report.findings.len(), 2);
    }

    #[test]
    fn notes_are_added_to_the_agent_report_when_there_is_one() {
        let mut report = report_with(vec![]);
        report.agents = Some(AgentReport::default());
        let outcome = ToolOutcome { findings: vec![], notes: vec!["n1".into()] };
        fold_into_report(&mut report, outcome, &GatePolicy::default());
        assert_eq!(report.agents.unwrap().notes, vec!["n1"]);
    }
}
