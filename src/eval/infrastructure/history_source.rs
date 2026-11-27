use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;

use crate::eval::application::{
    EvalError, FindingJudgment, ImportCriterion, ImportFinding, ImportTicket, ImportableRun, RunSource,
};
use crate::feedback::application::{
    CriterionVerdict, CriterionVerifications, FeedbackService, FindingFacts, Verdict, fingerprint,
};
use crate::history::application::{FindingView, RunRepository};

pub struct HistoryRunSource {
    runs: Arc<dyn RunRepository>,
    feedback: Arc<FeedbackService>,
    criteria: Arc<CriterionVerifications>,
}

impl HistoryRunSource {
    pub fn new(
        runs: Arc<dyn RunRepository>,
        feedback: Arc<FeedbackService>,
        criteria: Arc<CriterionVerifications>,
    ) -> Self {
        Self { runs, feedback, criteria }
    }
}

fn import_error(message: impl Into<String>) -> EvalError {
    EvalError::Import(message.into())
}

fn facts_of(finding: &FindingView) -> FindingFacts {
    let source = match &finding.source {
        Some(source) if source.kind != "llm" => format!("tool:{}", source.name.as_deref().unwrap_or_default()),
        _ => "llm".to_string(),
    };
    FindingFacts {
        file: finding.file.clone(),
        category: finding.category.as_str().to_string(),
        source,
        evidence: finding.evidence.clone(),
    }
}

impl RunSource for HistoryRunSource {
    fn load(&self, run_id: i64) -> Result<ImportableRun, EvalError> {
        let run = self
            .runs
            .get(run_id)
            .map_err(|e| import_error(e.to_string()))?
            .ok_or_else(|| EvalError::NotFound(format!("run {run_id} does not exist in the history")))?;
        let report =
            run.report.ok_or_else(|| import_error(format!("run {run_id} has no report (it ended in an error)")))?;
        let commit = run.commit.ok_or_else(|| import_error(format!("run {run_id} did not record a commit")))?;
        let repo = run.repo_path.clone();

        let prints: Vec<String> = report.findings.iter().map(|f| fingerprint(&facts_of(f))).collect();
        let now = Utc::now();
        let verdicts: HashMap<String, Verdict> = self
            .feedback
            .judgments(&repo, &prints)
            .map_err(|e| import_error(e.to_string()))?
            .into_iter()
            .filter_map(|j| j.effective_verdict(now).map(|verdict| (j.fingerprint, verdict)))
            .collect();
        let findings = report
            .findings
            .iter()
            .zip(&prints)
            .map(|(finding, print)| ImportFinding {
                file: finding.file.clone(),
                line: finding.line,
                category: finding.category,
                severity: finding.severity,
                judgment: verdicts.get(print).map(|verdict| match verdict {
                    Verdict::Accepted => FindingJudgment::Accepted,
                    Verdict::Dismissed => FindingJudgment::Dismissed,
                }),
            })
            .collect();

        let (ticket, criteria) = match &report.requirements {
            Some(requirements) => {
                let pairs: Vec<(String, String)> =
                    requirements.requirements.criteria.iter().map(|c| (c.id.clone(), c.text.clone())).collect();
                let found = self.criteria.lookup(&repo, &pairs).map_err(|e| import_error(e.to_string()))?;
                let criteria = pairs
                    .into_iter()
                    .zip(found)
                    .map(|((_, text), verification)| ImportCriterion {
                        text,
                        human_verified_met: verification.is_some_and(|v| v.verdict == CriterionVerdict::VerifiedMet),
                    })
                    .collect();
                let ticket = ImportTicket {
                    key: requirements.requirements.key.clone(),
                    title: requirements.requirements.title.clone(),
                };
                (Some(ticket), criteria)
            }
            None => (None, Vec::new()),
        };
        Ok(ImportableRun { run_id, repo_path: PathBuf::from(repo), commit, ticket, criteria, findings })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::application::Scope;
    use crate::feedback::infrastructure::SqliteFeedbackStore;
    use crate::history::application::{FindingSourceView, RecordRun};
    use crate::history::domain::fixtures::{at, live_run};
    use crate::history::infrastructure::SqliteRunRepository;

    struct Fixture {
        _dir: tempfile::TempDir,
        runs: Arc<dyn RunRepository>,
        feedback: Arc<FeedbackService>,
        criteria: Arc<CriterionVerifications>,
        run_id: i64,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let runs: Arc<dyn RunRepository> = Arc::new(SqliteRunRepository::open(dir.path().join("q.db")).unwrap());
        let store = Arc::new(SqliteFeedbackStore::open(dir.path().join("q.db")).unwrap());
        let run_id = RecordRun::new(runs.clone()).execute(&live_run("shop", at(1, 1))).unwrap();
        Fixture {
            runs,
            feedback: Arc::new(FeedbackService::new(store.clone())),
            criteria: Arc::new(CriterionVerifications::new(store)),
            run_id,
            _dir: dir,
        }
    }

    impl Fixture {
        fn source(&self) -> HistoryRunSource {
            HistoryRunSource::new(self.runs.clone(), self.feedback.clone(), self.criteria.clone())
        }

        fn stored_finding(&self) -> FindingView {
            self.runs.get(self.run_id).unwrap().unwrap().report.unwrap().findings.remove(0)
        }
    }

    #[test]
    fn findings_carry_the_judgment_recorded_for_their_fingerprint() {
        let fixture = fixture();
        let facts = facts_of(&fixture.stored_finding());
        fixture.feedback.record("/src/shop", &facts, Verdict::Accepted, Scope::Repo).unwrap();

        let run = fixture.source().load(fixture.run_id).unwrap();

        assert_eq!(run.findings[0].judgment, Some(FindingJudgment::Accepted));
        assert_eq!(run.commit, "abcdef0123456789");
        assert_eq!(run.repo_path, PathBuf::from("/src/shop"));
        assert_eq!(run.ticket, Some(ImportTicket { key: "QG-7".into(), title: "Login".into() }));
    }

    #[test]
    fn dismissed_and_unjudged_findings_are_told_apart() {
        let fixture = fixture();
        assert_eq!(fixture.source().load(fixture.run_id).unwrap().findings[0].judgment, None);

        fixture
            .feedback
            .record("/src/shop", &facts_of(&fixture.stored_finding()), Verdict::Dismissed, Scope::Repo)
            .unwrap();

        assert_eq!(
            fixture.source().load(fixture.run_id).unwrap().findings[0].judgment,
            Some(FindingJudgment::Dismissed)
        );
    }

    #[test]
    fn criteria_are_met_only_when_a_human_verified_them() {
        let fixture = fixture();
        assert!(!fixture.source().load(fixture.run_id).unwrap().criteria[0].human_verified_met);

        fixture.criteria.verify("/src/shop", "AC1", "Users can log in", CriterionVerdict::VerifiedMet, "").unwrap();

        let criteria = fixture.source().load(fixture.run_id).unwrap().criteria;
        assert_eq!(criteria, [ImportCriterion { text: "Users can log in".into(), human_verified_met: true }]);
    }

    #[test]
    fn a_not_applicable_verification_does_not_count_as_met() {
        let fixture = fixture();
        fixture.criteria.verify("/src/shop", "AC1", "Users can log in", CriterionVerdict::NotApplicable, "").unwrap();

        assert!(!fixture.source().load(fixture.run_id).unwrap().criteria[0].human_verified_met);
    }

    #[test]
    fn missing_runs_and_runs_without_a_report_or_commit_are_reported() {
        let fixture = fixture();
        let errored = RecordRun::new(fixture.runs.clone())
            .execute(&crate::history::domain::ReviewRun { report: None, ..live_run("shop", at(2, 1)) })
            .unwrap();
        let no_commit = RecordRun::new(fixture.runs.clone())
            .execute(&crate::history::domain::ReviewRun { commit: None, ..live_run("shop", at(3, 1)) })
            .unwrap();
        let source = fixture.source();

        assert!(matches!(source.load(999), Err(EvalError::NotFound(_))));
        assert!(matches!(source.load(errored), Err(EvalError::Import(m)) if m.contains("no report")));
        assert!(matches!(source.load(no_commit), Err(EvalError::Import(m)) if m.contains("commit")));
    }

    #[test]
    fn tool_findings_use_the_tool_scoped_source_like_the_review_does() {
        let mut finding = fixture().stored_finding();
        finding.source = Some(FindingSourceView { kind: "static_tool".into(), name: Some("gitleaks".into()) });

        assert_eq!(facts_of(&finding).source, "tool:gitleaks");
        finding.source = None;
        assert_eq!(facts_of(&finding).source, "llm");
    }
}
