use std::sync::Arc;

use super::ports::{CaseRepository, ChangeExporter, EvalError, FindingJudgment, ImportableRun, NewCase, RunSource};
use crate::eval::domain::case::{
    CaseCriterion, CaseTicket, DEFAULT_LINE_TOLERANCE, ExpectedFinding, ExpectedStatus, ForbiddenFinding, GoldenCase,
    language_of, validate_case_id,
};

const MAX_ID_LEN_FOR_SLUG: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedCase {
    pub id: String,
    pub expected: usize,
    pub forbidden: usize,
    pub labelled_criteria: usize,
    pub unlabelled_criteria: usize,
}

pub struct ImportRun {
    runs: Arc<dyn RunSource>,
    exporter: Arc<dyn ChangeExporter>,
    cases: Arc<dyn CaseRepository>,
}

impl ImportRun {
    pub fn new(runs: Arc<dyn RunSource>, exporter: Arc<dyn ChangeExporter>, cases: Arc<dyn CaseRepository>) -> Self {
        Self { runs, exporter, cases }
    }

    pub async fn execute(&self, run_id: i64, id: Option<String>, base_branch: &str) -> Result<ImportedCase, EvalError> {
        let run = self.runs.load(run_id)?;
        let id = id.unwrap_or_else(|| default_id(&run));
        validate_case_id(&id)?;
        let change = self.exporter.export(&run.repo_path, &run.commit, base_branch).await?;
        let paths: Vec<String> = change.base_files.iter().map(|f| f.path.clone()).collect();
        let case = build_case(&run, id, language_of(&paths))?;
        let summary = ImportedCase {
            id: case.id.clone(),
            expected: case.expected.len(),
            forbidden: case.forbidden.len(),
            labelled_criteria: case.labelled_criteria(),
            unlabelled_criteria: case.criteria.len() - case.labelled_criteria(),
        };
        self.cases.write(&NewCase { case, base_files: change.base_files, patch: change.patch })?;
        Ok(summary)
    }
}

fn build_case(run: &ImportableRun, id: String, language: String) -> Result<GoldenCase, EvalError> {
    let judged = |wanted: FindingJudgment| run.findings.iter().filter(move |f| f.judgment == Some(wanted));
    let expected = judged(FindingJudgment::Accepted)
        .map(|f| ExpectedFinding {
            file: f.file.clone(),
            line: f.line,
            line_tolerance: DEFAULT_LINE_TOLERANCE,
            category: f.category,
            min_severity: None,
        })
        .collect();
    let forbidden = judged(FindingJudgment::Dismissed)
        .map(|f| ForbiddenFinding {
            file: f.file.clone(),
            line: f.line,
            line_tolerance: DEFAULT_LINE_TOLERANCE,
            category: Some(f.category),
            min_severity: None,
        })
        .collect();
    let ticket = run.ticket.as_ref().filter(|_| !run.criteria.is_empty()).map(|ticket| CaseTicket {
        key: ticket.key.clone(),
        title: ticket.title.clone(),
        description: String::new(),
    });
    let criteria = if ticket.is_some() {
        run.criteria
            .iter()
            .map(|c| CaseCriterion {
                text: c.text.clone(),
                expect: c.human_verified_met.then_some(ExpectedStatus::Met),
            })
            .collect()
    } else {
        Vec::new()
    };
    let case = GoldenCase {
        id,
        description: format!("Imported from run {}", run.run_id),
        language,
        ticket,
        criteria,
        expected,
        forbidden,
    };
    if case.expected.is_empty() && case.forbidden.is_empty() && case.labelled_criteria() == 0 {
        return Err(EvalError::Import(format!(
            "run {} has nothing to label: accept or dismiss findings, or verify criteria as met, in the dashboard first",
            run.run_id
        )));
    }
    Ok(case)
}

fn default_id(run: &ImportableRun) -> String {
    let suffix = run.ticket.as_ref().map(|t| slug(&t.key)).filter(|s| !s.is_empty());
    match suffix {
        Some(suffix) => format!("run-{}-{suffix}", run.run_id),
        None => format!("run-{}", run.run_id),
    }
}

fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.truncate(MAX_ID_LEN_FOR_SLUG);
    slug.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use futures::future::BoxFuture;

    use super::*;
    use crate::eval::application::ports::{BaseFile, ExportedChange, ImportCriterion, ImportFinding, ImportTicket};
    use crate::eval::application::test_support::FakeCases;
    use crate::review::application::{Category, Severity};

    struct FakeRuns(Result<ImportableRun, EvalError>);

    impl RunSource for FakeRuns {
        fn load(&self, _run_id: i64) -> Result<ImportableRun, EvalError> {
            self.0.clone()
        }
    }

    #[derive(Default)]
    struct FakeExporter {
        calls: std::sync::Mutex<Vec<(PathBuf, String, String)>>,
    }

    impl ChangeExporter for FakeExporter {
        fn export<'a>(
            &'a self,
            repo: &'a Path,
            commit: &'a str,
            base_branch: &'a str,
        ) -> BoxFuture<'a, Result<ExportedChange, EvalError>> {
            self.calls.lock().unwrap().push((repo.to_path_buf(), commit.into(), base_branch.into()));
            Box::pin(async {
                Ok(ExportedChange {
                    base_files: vec![BaseFile { path: "main.go".into(), bytes: b"package main".to_vec() }],
                    patch: "diff".into(),
                })
            })
        }
    }

    fn finding(line: u32, judgment: Option<FindingJudgment>) -> ImportFinding {
        ImportFinding {
            file: "main.go".into(),
            line,
            category: Category::Security,
            severity: Severity::Major,
            judgment,
        }
    }

    fn run(findings: Vec<ImportFinding>, criteria: Vec<ImportCriterion>) -> ImportableRun {
        ImportableRun {
            run_id: 42,
            repo_path: PathBuf::from("/repos/shop"),
            commit: "abc1234".into(),
            ticket: Some(ImportTicket { key: "KAN-5".into(), title: "Fix".into() }),
            criteria,
            findings,
        }
    }

    fn criterion(text: &str, met: bool) -> ImportCriterion {
        ImportCriterion { text: text.into(), human_verified_met: met }
    }

    async fn import(
        run: ImportableRun,
        id: Option<&str>,
    ) -> (Result<ImportedCase, EvalError>, Arc<FakeCases>, Arc<FakeExporter>) {
        let cases = Arc::new(FakeCases::new(&[]));
        let exporter = Arc::new(FakeExporter::default());
        let use_case = ImportRun::new(Arc::new(FakeRuns(Ok(run))), exporter.clone(), cases.clone());
        let result = use_case.execute(42, id.map(String::from), "main").await;
        (result, cases, exporter)
    }

    #[tokio::test]
    async fn accepted_findings_become_expected_and_dismissed_ones_forbidden() {
        let findings = vec![
            finding(10, Some(FindingJudgment::Accepted)),
            finding(20, Some(FindingJudgment::Dismissed)),
            finding(30, None),
        ];

        let (result, cases, exporter) = import(run(findings, vec![]), None).await;

        let summary = result.unwrap();
        assert_eq!(
            summary,
            ImportedCase {
                id: "run-42-kan-5".into(),
                expected: 1,
                forbidden: 1,
                labelled_criteria: 0,
                unlabelled_criteria: 0
            }
        );
        let written = cases.written.lock().unwrap();
        assert_eq!(written[0].case.expected[0].line, 10);
        assert_eq!(written[0].case.forbidden[0].category, Some(Category::Security));
        assert_eq!(written[0].case.language, "go");
        assert_eq!(written[0].patch, "diff");
        assert_eq!(exporter.calls.lock().unwrap()[0], (PathBuf::from("/repos/shop"), "abc1234".into(), "main".into()));
    }

    #[tokio::test]
    async fn only_human_verified_met_criteria_are_labelled() {
        let criteria = vec![criterion("a", true), criterion("b", false)];

        let (result, cases, _) = import(run(vec![finding(1, Some(FindingJudgment::Accepted))], criteria), None).await;

        let summary = result.unwrap();
        assert_eq!((summary.labelled_criteria, summary.unlabelled_criteria), (1, 1));
        let written = cases.written.lock().unwrap();
        assert_eq!(written[0].case.criteria[0].expect, Some(ExpectedStatus::Met));
        assert_eq!(written[0].case.criteria[1].expect, None);
        assert_eq!(written[0].case.ticket.as_ref().unwrap().key, "KAN-5");
    }

    #[tokio::test]
    async fn a_run_with_nothing_judged_is_refused() {
        let (result, cases, _) = import(run(vec![finding(1, None)], vec![criterion("a", false)]), None).await;

        assert!(matches!(result, Err(EvalError::Import(m)) if m.contains("nothing to label")));
        assert!(cases.written.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_id_is_validated_and_existing_cases_are_not_overwritten() {
        let judged = || run(vec![finding(1, Some(FindingJudgment::Accepted))], vec![]);

        let (bad, _, exporter) = import(judged(), Some("../evil")).await;
        assert!(matches!(bad, Err(EvalError::Case(_))));
        assert!(exporter.calls.lock().unwrap().is_empty(), "nothing is exported for an invalid id");

        let cases = Arc::new(FakeCases::new(&[("taken", "")]));
        let use_case = ImportRun::new(Arc::new(FakeRuns(Ok(judged()))), Arc::new(FakeExporter::default()), cases);
        let clash = use_case.execute(42, Some("taken".into()), "main").await;
        assert!(matches!(clash, Err(EvalError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn run_source_errors_propagate() {
        let use_case = ImportRun::new(
            Arc::new(FakeRuns(Err(EvalError::NotFound("run 42".into())))),
            Arc::new(FakeExporter::default()),
            Arc::new(FakeCases::new(&[])),
        );

        assert!(matches!(use_case.execute(42, None, "main").await, Err(EvalError::NotFound(_))));
    }

    #[test]
    fn default_ids_are_slugged() {
        let mut importable = run(vec![], vec![]);
        assert_eq!(default_id(&importable), "run-42-kan-5");
        importable.ticket = Some(ImportTicket { key: "  Weird / Key!! ".into(), title: String::new() });
        assert_eq!(default_id(&importable), "run-42-weird-key");
        importable.ticket = None;
        assert_eq!(default_id(&importable), "run-42");
    }
}
