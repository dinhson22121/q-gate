use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;

use super::finding_mapper::quality_finding_to_finding;
use crate::conventions::application::{Rule, RuleStatus, Rulebook};
use crate::quality::{
    ChangedFile, ImportBoundary, Language as QualityLanguage, QualityConfig, QualityRequest as ToolRequest,
    ToolchainPaths, parse_boundary_directives,
};
use crate::review::application::{QualityChecks, QualityRequest, ToolOutcome};
use crate::review::domain::change_set::{ChangeSet, ChangeStatus, DiffLine};
use crate::review::domain::language::Language;
use crate::sandbox::application::ExecBackend;

const TOOLCHAINS_DIR: &str = "/toolchains";

pub struct QualityTools {
    exec: Arc<dyn ExecBackend>,
    languages: Vec<QualityLanguage>,
    boundaries: Vec<ImportBoundary>,
    toolchains: ToolchainPaths,
}

impl QualityTools {
    pub fn new(
        exec: Arc<dyn ExecBackend>,
        languages: &[Language],
        boundaries: Vec<ImportBoundary>,
        toolchains: ToolchainPaths,
    ) -> Self {
        Self { exec, languages: quality_languages(languages), boundaries, toolchains }
    }

    pub fn discover_toolchains() -> ToolchainPaths {
        ToolchainPaths::discover(Path::new(TOOLCHAINS_DIR), std::env::var("PATH").ok().as_deref())
    }
}

impl QualityChecks for QualityTools {
    fn run<'a>(&'a self, request: &'a QualityRequest<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let config = QualityConfig::new(self.languages.iter().copied(), self.toolchains.clone())
                .with_boundaries(self.boundaries.clone());
            let tool_request = ToolRequest {
                snapshot: request.snapshot.to_path_buf(),
                base_sha: request.base_sha.map(String::from),
                head_sha: request.head_sha.to_string(),
                changed_files: changed_files(request.changes),
            };
            let report = crate::quality::build(self.exec.clone(), config).execute(&tool_request).await;
            ToolOutcome {
                findings: report.findings.iter().map(quality_finding_to_finding).collect(),
                notes: report.notes,
            }
        })
    }
}

pub fn rulebook_boundaries<E: Display>(
    lookup: Result<Option<Rulebook>, E>,
    use_inferred_rules: bool,
) -> Vec<ImportBoundary> {
    match lookup {
        Ok(Some(rulebook)) => boundaries_from_rulebook(&rulebook, use_inferred_rules),
        Ok(None) => Vec::new(),
        Err(e) => {
            eprintln!("qgate: warning: cannot read the rulebook for architecture boundaries: {e}");
            Vec::new()
        }
    }
}

fn boundaries_from_rulebook(rulebook: &Rulebook, use_inferred_rules: bool) -> Vec<ImportBoundary> {
    rulebook.rules.iter().filter(|rule| rule.is_usable(use_inferred_rules)).flat_map(rule_boundaries).collect()
}

fn rule_boundaries(rule: &Rule) -> Vec<ImportBoundary> {
    let mut texts = vec![rule.description.as_str()];
    texts.extend(rule.lint_suggestion.as_deref());
    parse_boundary_directives(&rule.id, &rule.title, rule.status == RuleStatus::Approved, &texts)
}

fn quality_languages(configured: &[Language]) -> Vec<QualityLanguage> {
    let mut languages: Vec<QualityLanguage> = configured
        .iter()
        .filter_map(|l| match l {
            Language::Go => Some(QualityLanguage::Go),
            Language::Java => Some(QualityLanguage::Java),
            _ => None,
        })
        .collect();
    languages.dedup();
    languages
}

fn changed_files(changes: &ChangeSet) -> Vec<ChangedFile> {
    changes
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
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::conventions::application::RuleKind;
    use crate::review::domain::change_set::{FileChange, Hunk};
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
        let changed = changed_files(&sample_changes());
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
    fn a_missing_or_unreadable_rulebook_yields_no_boundaries() {
        assert!(rulebook_boundaries(Ok::<_, String>(None), true).is_empty());
        assert!(rulebook_boundaries(Err::<Option<Rulebook>, _>("locked"), true).is_empty());
    }

    #[test]
    fn only_languages_the_tools_support_are_selected() {
        let configured = [Language::Go, Language::Python, Language::Java, Language::Rust];
        assert_eq!(quality_languages(&configured), vec![QualityLanguage::Go, QualityLanguage::Java]);
        assert!(quality_languages(&[]).is_empty());
    }

    #[tokio::test]
    async fn missing_toolchains_become_notes_not_findings() {
        let dir = tempfile::tempdir().unwrap();
        let tools = QualityTools::new(
            Arc::new(RecordingBackend::answering(ExecResult::default())),
            &[Language::Go],
            Vec::new(),
            ToolchainPaths::default(),
        );
        let changes = sample_changes();
        let request =
            QualityRequest { snapshot: dir.path(), base_sha: None, head_sha: &"h".repeat(40), changes: &changes };
        let outcome = tools.run(&request).await;
        assert!(outcome.findings.is_empty());
        assert!(outcome.notes.iter().any(|n| n.contains("not installed")), "{:?}", outcome.notes);
    }
}
