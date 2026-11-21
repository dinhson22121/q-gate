//! Use case: review a set of code changes and decide whether they pass the gate.

use futures::future::BoxFuture;
use futures::{StreamExt, stream};
use globset::{Glob, GlobSet, GlobSetBuilder};

use super::batching::plan_batches;
use super::ports::{
    AssessmentOutput, ChangeSource, CheckRunner, CodeReviewer, RequirementsInput, ReviewInput, ReviewerError,
    ReviewerOutput, SourceError,
};
use super::run_agents::{AgentRun, run_agents};
use crate::review::domain::agent_results::{TicketContext, verify_agent_report};
use crate::review::domain::change_set::{ChangeSet, ChangeStatus, FileChange};
use crate::review::domain::checks::CheckSpec;
use crate::review::domain::finding::Finding;
use crate::review::domain::gate::{GateInput, GatePolicy};
use crate::review::domain::report::{ReviewReport, SkipReason, SkippedFile, TokenUsage};
use crate::review::domain::requirements::{CriterionAssessment, CriterionStatus, Requirements, verify_assessments};
use crate::review::domain::verification::verify_findings;

#[derive(Debug, Clone)]
pub struct ReviewSettings {
    pub ignore_patterns: Vec<String>,
    pub max_file_bytes: usize,
    pub max_batch_weight: usize,
    pub max_files_per_batch: usize,
    pub concurrency: usize,
    pub guidelines: String,
    pub gate: GatePolicy,
    pub checks: Vec<CheckSpec>,
    /// When false (ticket in progress, e.g. a `[WIP]` tip commit), unmet acceptance criteria are
    /// shown as progress but do not block the gate: `fail_on_unmet_criteria` is treated as false
    /// for this run only, without mutating the stored policy.
    pub block_acceptance: bool,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            ignore_patterns: Vec::new(),
            max_file_bytes: 300 * 1024,
            max_batch_weight: 120_000,
            max_files_per_batch: 10,
            concurrency: 4,
            guidelines: String::new(),
            gate: GatePolicy::default(),
            checks: Vec::new(),
            block_acceptance: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Reviewer(#[from] ReviewerError),
    #[error("invalid ignore pattern '{pattern}': {reason}")]
    InvalidIgnorePattern { pattern: String, reason: String },
}

/// Called before the reviewer runs, so the interface can show progress.
pub trait ReviewProgress: Sync {
    fn started(&self, files: usize, batches: usize);
}

pub struct NoProgress;

impl ReviewProgress for NoProgress {
    fn started(&self, _files: usize, _batches: usize) {}
}

pub struct ReviewChanges<S, R, C> {
    source: S,
    reviewer: R,
    checks: C,
    settings: ReviewSettings,
    ignore: GlobSet,
}

#[derive(Default)]
struct BatchOutcome {
    findings: Vec<Finding>,
    usage: TokenUsage,
    cost_usd: Option<f64>,
    failed: Vec<SkippedFile>,
    fatal: Option<ReviewerError>,
}

impl BatchOutcome {
    fn success(output: ReviewerOutput) -> Self {
        Self { findings: output.findings, usage: output.usage, cost_usd: output.cost_usd, ..Self::default() }
    }

    fn failed(files: &[FileChange], error: &ReviewerError) -> Self {
        let failed = files
            .iter()
            .map(|f| SkippedFile { path: f.path.clone(), reason: SkipReason::ReviewFailed(error.to_string()) })
            .collect();
        Self { failed, ..Self::default() }
    }

    fn merge(mut self, other: Self) -> Self {
        self.findings.extend(other.findings);
        self.usage += other.usage;
        self.cost_usd = add_costs(self.cost_usd, other.cost_usd);
        self.failed.extend(other.failed);
        self.fatal = self.fatal.or(other.fatal);
        self
    }
}

fn add_costs(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

impl<S, R, C> ReviewChanges<S, R, C>
where
    S: ChangeSource + Sync,
    R: CodeReviewer + Sync,
    C: CheckRunner + Sync,
{
    pub fn new(source: S, reviewer: R, checks: C, settings: ReviewSettings) -> Result<Self, ReviewError> {
        let ignore = build_globset(&settings.ignore_patterns)?;
        Ok(Self { source, reviewer, checks, settings, ignore })
    }

    /// The gate for this run: a ticket in progress relaxes `fail_on_unmet_criteria` to false, so
    /// acceptance criteria are reported as progress without blocking. The stored policy is unchanged.
    fn effective_gate(&self) -> GatePolicy {
        if self.settings.block_acceptance {
            self.settings.gate.clone()
        } else {
            GatePolicy { fail_on_unmet_criteria: false, ..self.settings.gate.clone() }
        }
    }

    /// Runs checks, defect review and (when `requirements` is given) the acceptance-criteria
    /// assessment concurrently, verifies every claim against the code and applies the gate.
    pub async fn execute(
        &self,
        requirements: Option<Requirements>,
        progress: &dyn ReviewProgress,
    ) -> Result<ReviewReport, ReviewError> {
        self.execute_with_agents(requirements, None, None, progress).await
    }

    /// Like [`execute`](Self::execute), plus the ticket agents (tests, architecture, design) when
    /// `agents` is given. Agent results are verified and go through the same gate.
    pub async fn execute_with_agents(
        &self,
        requirements: Option<Requirements>,
        ticket: Option<TicketContext>,
        agents: Option<AgentRun<'_>>,
        progress: &dyn ReviewProgress,
    ) -> Result<ReviewReport, ReviewError> {
        let changes = self.source.load().await?;
        let (reviewable, mut skipped) = self.select(changes.files);
        if reviewable.is_empty() {
            let mut report = self.empty_report(skipped, requirements);
            report.ticket = ticket;
            return Ok(report);
        }
        let batches = plan_batches(&reviewable, self.settings.max_batch_weight, self.settings.max_files_per_batch);
        progress.started(reviewable.len(), batches.len());

        let review = stream::iter(batches)
            .map(|range| self.review_batch(&reviewable[range]))
            .buffer_unordered(self.settings.concurrency.max(1))
            .fold(BatchOutcome::default(), |acc, next| async move { acc.merge(next) });
        let assessment = self.assess(&reviewable, requirements.as_ref());
        let agent_work = async {
            match &agents {
                Some(run) if !run.suite.is_empty() => Some(run_agents(run, &reviewable, requirements.as_ref()).await),
                _ => None,
            }
        };
        let (outcome, assessment, check_results, agent_report) =
            futures::join!(review, assessment, self.checks.run(&self.settings.checks), agent_work);
        if let Some(fatal) = outcome.fatal {
            return Err(fatal.into());
        }
        let assessment = assessment?;

        let failed_paths: Vec<&str> = outcome.failed.iter().map(|s| s.path.as_str()).collect();
        let files_reviewed =
            reviewable.iter().map(|f| f.path.clone()).filter(|p| !failed_paths.contains(&p.as_str())).collect();
        skipped.extend(outcome.failed);

        let change_set = ChangeSet { files: reviewable };
        let verification = verify_findings(&change_set, outcome.findings);
        let requirements_report = requirements.map(|r| verify_assessments(&change_set, r, assessment.assessments));
        let incomplete = skipped.iter().any(|s| matches!(s.reason, SkipReason::ReviewFailed(_)));
        let agent_report = agent_report.map(|report| verify_agent_report(&change_set, report));
        let gate = self.effective_gate().decide(GateInput {
            findings: &verification.accepted,
            requirements: requirements_report.as_ref(),
            checks: &check_results,
            incomplete,
            agents: agent_report.as_ref(),
        });
        let mut usage = outcome.usage;
        usage += assessment.usage;
        Ok(ReviewReport {
            files_reviewed,
            skipped,
            findings: verification.accepted,
            rejected: verification.rejected,
            requirements: requirements_report,
            checks: check_results,
            agents: agent_report,
            ticket,
            gate,
            usage,
            estimated_cost_usd: add_costs(outcome.cost_usd, assessment.cost_usd),
        })
    }

    /// Nothing reviewable: no model call and no checks; criteria cannot be judged.
    fn empty_report(&self, skipped: Vec<SkippedFile>, requirements: Option<Requirements>) -> ReviewReport {
        let requirements_report = requirements.map(|r| verify_assessments(&ChangeSet::default(), r, Vec::new()));
        let gate = self.effective_gate().decide(GateInput {
            findings: &[],
            requirements: requirements_report.as_ref(),
            checks: &[],
            incomplete: false,
            agents: None,
        });
        ReviewReport {
            files_reviewed: Vec::new(),
            skipped,
            findings: Vec::new(),
            rejected: Vec::new(),
            requirements: requirements_report,
            checks: Vec::new(),
            agents: None,
            ticket: None,
            gate,
            usage: TokenUsage::default(),
            estimated_cost_usd: None,
        }
    }

    /// A non-fatal assessment failure leaves every criterion `Unclear` with the reason.
    async fn assess(
        &self,
        files: &[FileChange],
        requirements: Option<&Requirements>,
    ) -> Result<AssessmentOutput, ReviewerError> {
        let Some(requirements) = requirements else {
            return Ok(AssessmentOutput::default());
        };
        match self.reviewer.assess_requirements(RequirementsInput { files, requirements }).await {
            Ok(output) => Ok(output),
            Err(error) if error.is_fatal() => Err(error),
            Err(error) => Ok(AssessmentOutput {
                assessments: requirements
                    .criteria
                    .iter()
                    .map(|c| CriterionAssessment {
                        criterion_id: c.id.clone(),
                        status: CriterionStatus::Unclear,
                        explanation: format!("Assessment failed: {error}"),
                        evidence: Vec::new(),
                    })
                    .collect(),
                ..AssessmentOutput::default()
            }),
        }
    }

    fn select(&self, files: Vec<FileChange>) -> (Vec<FileChange>, Vec<SkippedFile>) {
        let mut reviewable = Vec::new();
        let mut skipped = Vec::new();
        for file in files {
            match self.skip_reason(&file) {
                Some(reason) => skipped.push(SkippedFile { path: file.path, reason }),
                None => reviewable.push(file),
            }
        }
        (reviewable, skipped)
    }

    fn skip_reason(&self, file: &FileChange) -> Option<SkipReason> {
        if file.status == ChangeStatus::Deleted {
            Some(SkipReason::Deleted)
        } else if file.is_binary {
            Some(SkipReason::Binary)
        } else if self.ignore.is_match(&file.path) {
            Some(SkipReason::Ignored)
        } else if file.new_content.as_ref().is_some_and(|c| c.len() > self.settings.max_file_bytes) {
            Some(SkipReason::TooLarge)
        } else if !file.has_content_change() {
            Some(SkipReason::NoContentChange)
        } else {
            None
        }
    }

    /// Reviews one batch; a truncated response is retried as two smaller batches.
    fn review_batch<'a>(&'a self, files: &'a [FileChange]) -> BoxFuture<'a, BatchOutcome> {
        Box::pin(async move {
            let input = ReviewInput { files, guidelines: &self.settings.guidelines };
            match self.reviewer.review(input).await {
                Ok(output) => BatchOutcome::success(output),
                Err(ReviewerError::Truncated) if files.len() > 1 => {
                    let (left, right) = files.split_at(files.len() / 2);
                    let (a, b) = futures::join!(self.review_batch(left), self.review_batch(right));
                    a.merge(b)
                }
                Err(error) if error.is_fatal() => BatchOutcome { fatal: Some(error), ..BatchOutcome::default() },
                Err(error) => BatchOutcome::failed(files, &error),
            }
        })
    }
}

fn build_globset(patterns: &[String]) -> Result<GlobSet, ReviewError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern)
            .map_err(|e| ReviewError::InvalidIgnorePattern { pattern: pattern.clone(), reason: e.to_string() })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| ReviewError::InvalidIgnorePattern { pattern: patterns.join(", "), reason: e.to_string() })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::review::domain::change_set::fixtures::{added, file};
    use crate::review::domain::checks::{CheckOutcome, CheckResult};
    use crate::review::domain::finding::Severity;
    use crate::review::domain::finding::fixtures::finding;
    use crate::review::domain::requirements::{AcceptanceCriterion, EvidenceRef};

    struct FakeSource(ChangeSet);

    impl ChangeSource for FakeSource {
        async fn load(&self) -> Result<ChangeSet, SourceError> {
            Ok(self.0.clone())
        }
    }

    /// Returns scripted results; records the batch sizes it was called with.
    struct FakeReviewer {
        script: Mutex<Vec<Result<ReviewerOutput, ReviewerError>>>,
        calls: Mutex<Vec<usize>>,
        assessment_error: Mutex<Option<ReviewerError>>,
    }

    impl FakeReviewer {
        fn new(mut script: Vec<Result<ReviewerOutput, ReviewerError>>) -> Self {
            script.reverse();
            Self { script: Mutex::new(script), calls: Mutex::new(Vec::new()), assessment_error: Mutex::new(None) }
        }
    }

    impl CodeReviewer for FakeReviewer {
        async fn review(&self, input: ReviewInput<'_>) -> Result<ReviewerOutput, ReviewerError> {
            self.calls.lock().unwrap().push(input.files.len());
            self.script.lock().unwrap().pop().unwrap_or_else(|| Ok(ReviewerOutput::default()))
        }

        async fn assess_requirements(&self, input: RequirementsInput<'_>) -> Result<AssessmentOutput, ReviewerError> {
            if let Some(error) = self.assessment_error.lock().unwrap().take() {
                return Err(error);
            }
            let id = input.requirements.criteria[0].id.clone();
            Ok(AssessmentOutput {
                assessments: vec![CriterionAssessment {
                    criterion_id: id,
                    status: CriterionStatus::Met,
                    explanation: "done".into(),
                    evidence: vec![EvidenceRef { file: "a.go".into(), line: 1, quote: "total := compute(1)".into() }],
                }],
                usage: TokenUsage { input_tokens: 7, ..TokenUsage::default() },
                cost_usd: Some(0.01),
            })
        }
    }

    struct FakeChecks(Vec<CheckResult>);

    impl CheckRunner for FakeChecks {
        async fn run(&self, _checks: &[CheckSpec]) -> Vec<CheckResult> {
            self.0.clone()
        }
    }

    fn use_case(
        files: Vec<FileChange>,
        reviewer: FakeReviewer,
        settings: ReviewSettings,
    ) -> ReviewChanges<FakeSource, FakeReviewer, FakeChecks> {
        ReviewChanges::new(FakeSource(ChangeSet { files }), reviewer, FakeChecks(vec![]), settings).unwrap()
    }

    fn requirements(ids: &[&str]) -> Requirements {
        Requirements {
            key: "ABC-1".into(),
            title: "t".into(),
            url: None,
            criteria: ids.iter().map(|id| AcceptanceCriterion { id: (*id).into(), text: "do it".into() }).collect(),
        }
    }

    fn go_file(path: &str) -> FileChange {
        file(path, 1, vec![added(1, "total := compute(1)")], Some("total := compute(1)\n"))
    }

    fn output(findings: Vec<Finding>, cost: f64) -> ReviewerOutput {
        ReviewerOutput {
            findings,
            usage: TokenUsage { input_tokens: 10, output_tokens: 5, ..TokenUsage::default() },
            cost_usd: Some(cost),
        }
    }

    fn sequential() -> ReviewSettings {
        ReviewSettings { concurrency: 1, ..ReviewSettings::default() }
    }

    #[tokio::test]
    async fn verified_critical_finding_fails_gate() {
        let reviewer = FakeReviewer::new(vec![Ok(output(
            vec![
                finding("a.go", 1, Severity::Critical, "total := compute(1)"),
                finding("a.go", 1, Severity::Major, "invented()"),
            ],
            0.05,
        ))]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await.unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.rejected.len(), 1);
        assert!(!report.gate.passed);
        assert_eq!(report.files_reviewed, vec!["a.go"]);
        assert_eq!(report.usage.input_tokens, 10);
        assert_eq!(report.estimated_cost_usd, Some(0.05));
    }

    #[tokio::test]
    async fn skips_deleted_binary_ignored_and_large_files_without_calling_reviewer() {
        let mut deleted = go_file("gone.go");
        deleted.status = ChangeStatus::Deleted;
        let mut binary = go_file("img.png");
        binary.is_binary = true;
        let large = file("big.go", 1, vec![added(1, "x")], Some(&"x".repeat(50)));
        let unchanged = file("renamed.go", 1, vec![], None);
        let files = vec![deleted, binary, go_file("vendor/lib.go"), large, unchanged];
        let settings = ReviewSettings { ignore_patterns: vec!["vendor/**".into()], max_file_bytes: 10, ..sequential() };
        let reviewer = FakeReviewer::new(vec![]);
        let use_case = use_case(files, reviewer, settings);
        let report = use_case.execute(Some(requirements(&["AC-1"])), &NoProgress).await.unwrap();
        let reasons: Vec<_> = report.skipped.iter().map(|s| s.reason.clone()).collect();
        assert_eq!(
            reasons,
            vec![
                SkipReason::Deleted,
                SkipReason::Binary,
                SkipReason::Ignored,
                SkipReason::TooLarge,
                SkipReason::NoContentChange
            ]
        );
        assert!(use_case.reviewer.calls.lock().unwrap().is_empty());
        assert_eq!(report.requirements.unwrap().assessments[0].status, CriterionStatus::Unclear);
        assert!(report.gate.passed);
        assert_eq!(report.estimated_cost_usd, None);
    }

    #[tokio::test]
    async fn truncated_batch_is_split_and_retried() {
        let reviewer =
            FakeReviewer::new(vec![Err(ReviewerError::Truncated), Ok(output(vec![], 0.01)), Ok(output(vec![], 0.02))]);
        let use_case = use_case(vec![go_file("a.go"), go_file("b.go")], reviewer, sequential());
        let report = use_case.execute(None, &NoProgress).await.unwrap();
        assert_eq!(*use_case.reviewer.calls.lock().unwrap(), vec![2, 1, 1]);
        assert_eq!(report.files_reviewed.len(), 2);
        assert!((report.estimated_cost_usd.unwrap() - 0.03).abs() < 1e-9);
    }

    #[tokio::test]
    async fn refused_batch_marks_files_failed_and_report_incomplete() {
        let reviewer = FakeReviewer::new(vec![Err(ReviewerError::Refused { category: Some("cyber".into()) })]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await.unwrap();
        assert!(report.files_reviewed.is_empty());
        assert!(report.is_incomplete());
        assert!(report.gate.incomplete);
        assert!(!report.gate.passed, "an unreviewed change must not pass");
    }

    #[tokio::test]
    async fn fatal_reviewer_error_aborts_review() {
        let reviewer = FakeReviewer::new(vec![Err(ReviewerError::Rejected("invalid x-api-key".into()))]);
        let result = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await;
        assert!(matches!(result, Err(ReviewError::Reviewer(ReviewerError::Rejected(_)))));
    }

    #[test]
    fn invalid_ignore_pattern_is_reported() {
        let settings = ReviewSettings { ignore_patterns: vec!["a[".into()], ..ReviewSettings::default() };
        let result = ReviewChanges::new(
            FakeSource(ChangeSet::default()),
            FakeReviewer::new(vec![]),
            FakeChecks(vec![]),
            settings,
        );
        assert!(matches!(result, Err(ReviewError::InvalidIgnorePattern { .. })));
    }

    #[tokio::test]
    async fn ticket_in_progress_unmet_criterion_does_not_block_gate() {
        // A verified not-met criterion would normally block; in progress it only shows as progress.
        use crate::review::domain::requirements::{CriterionAssessment, CriterionStatus};
        struct NotMetReviewer;
        impl CodeReviewer for NotMetReviewer {
            async fn review(&self, _input: ReviewInput<'_>) -> Result<ReviewerOutput, ReviewerError> {
                Ok(ReviewerOutput::default())
            }
            async fn assess_requirements(
                &self,
                input: RequirementsInput<'_>,
            ) -> Result<AssessmentOutput, ReviewerError> {
                Ok(AssessmentOutput {
                    assessments: input
                        .requirements
                        .criteria
                        .iter()
                        .map(|c| CriterionAssessment {
                            criterion_id: c.id.clone(),
                            status: CriterionStatus::NotMet,
                            explanation: "not done yet".into(),
                            evidence: Vec::new(),
                        })
                        .collect(),
                    ..AssessmentOutput::default()
                })
            }
        }
        let files = vec![go_file("a.go")];
        let strict = ReviewChanges::new(
            FakeSource(ChangeSet { files: files.clone() }),
            NotMetReviewer,
            FakeChecks(vec![]),
            sequential(),
        )
        .unwrap();
        let blocked = strict.execute(Some(requirements(&["AC-1"])), &NoProgress).await.unwrap();
        assert!(!blocked.gate.passed, "a not-met criterion blocks by default");
        assert_eq!(blocked.gate.unmet_criteria, vec!["AC-1"]);

        let in_progress = ReviewChanges::new(
            FakeSource(ChangeSet { files }),
            NotMetReviewer,
            FakeChecks(vec![]),
            ReviewSettings { block_acceptance: false, ..sequential() },
        )
        .unwrap();
        let report = in_progress.execute(Some(requirements(&["AC-1"])), &NoProgress).await.unwrap();
        assert!(report.gate.passed, "in progress: shown but not blocking");
        assert_eq!(report.gate.unmet_criteria, vec!["AC-1"], "still reported as progress");
    }

    #[tokio::test]
    async fn requirements_are_assessed_verified_and_gate_fails_on_unmet() {
        let report = use_case(vec![go_file("a.go")], FakeReviewer::new(vec![]), sequential())
            .execute(Some(requirements(&["AC-1", "AC-2"])), &NoProgress)
            .await
            .unwrap();
        let req = report.requirements.as_ref().unwrap();
        assert_eq!(req.assessments[0].status, CriterionStatus::Met);
        assert_eq!(req.assessments[1].status, CriterionStatus::Unclear);
        assert_eq!(report.usage.input_tokens, 7);
        assert!(report.gate.passed, "unclear is not unmet");
    }

    #[tokio::test]
    async fn failed_assessment_marks_criteria_unclear_but_fatal_aborts() {
        let reviewer = FakeReviewer::new(vec![]);
        *reviewer.assessment_error.lock().unwrap() = Some(ReviewerError::Unavailable("overloaded".into()));
        let report = use_case(vec![go_file("a.go")], reviewer, sequential())
            .execute(Some(requirements(&["AC-1"])), &NoProgress)
            .await
            .unwrap();
        let assessment = &report.requirements.unwrap().assessments[0];
        assert_eq!(assessment.status, CriterionStatus::Unclear);
        assert!(assessment.explanation.contains("overloaded"));

        let reviewer = FakeReviewer::new(vec![]);
        *reviewer.assessment_error.lock().unwrap() = Some(ReviewerError::Rejected("401".into()));
        let result = use_case(vec![go_file("a.go")], reviewer, sequential())
            .execute(Some(requirements(&["AC-1"])), &NoProgress)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn failing_required_check_fails_gate() {
        let failing = CheckResult {
            name: "go test".into(),
            required: true,
            outcome: CheckOutcome::Failed,
            exit_code: Some(1),
            duration_ms: 5,
            output_tail: "FAIL".into(),
        };
        let use_case = ReviewChanges::new(
            FakeSource(ChangeSet { files: vec![go_file("a.go")] }),
            FakeReviewer::new(vec![]),
            FakeChecks(vec![failing]),
            sequential(),
        )
        .unwrap();
        let report = use_case.execute(None, &NoProgress).await.unwrap();
        assert!(!report.gate.passed);
        assert_eq!(report.gate.failed_checks, vec!["go test"]);
    }
}
