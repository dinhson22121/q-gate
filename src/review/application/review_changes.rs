use std::sync::Arc;

use futures::future::BoxFuture;
use futures::{StreamExt, stream};
use globset::{Glob, GlobSet, GlobSetBuilder};

use super::batching::plan_batches;
pub use super::ports::MAX_RETRY_CONTEXT_BYTES;
use super::ports::{
    AssessmentOutput, AuditInput, CallerFinder, ChangeSource, CheckRunner, CodeReviewer, FeedbackPolicy,
    FindingAuditor, HumanInput, HumanVerifications, RequirementsInput, ReviewInput, ReviewerError, ReviewerOutput,
    SourceError,
};
pub use super::retry_criteria::MAX_RETRY_CRITERIA;
use super::retry_criteria::retry_unverified;
use super::run_agents::{AgentRun, run_agents};
use crate::review::domain::agent_results::{TicketContext, verify_agent_report};
use crate::review::domain::callers::{CallerContext, CallerSite};
use crate::review::domain::change_set::{ChangeSet, ChangeStatus, FileChange};
use crate::review::domain::checks::CheckSpec;
use crate::review::domain::feedback::apply_feedback;
use crate::review::domain::finding::Finding;
use crate::review::domain::gate::{GateInput, GatePolicy};
use crate::review::domain::report::{ReviewReport, SkipReason, SkippedFile, TokenUsage};
use crate::review::domain::reproduce::demote_unproven;
use crate::review::domain::requirements::{CriterionAssessment, Requirements, apply_human_marks, verify_assessments};
use crate::review::domain::unverifiable::{UnverifiableReason, collect_unverifiable};
use crate::review::domain::verification::{Verification, apply_audit, verify_findings};

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
    auditor: Option<Arc<dyn FindingAuditor>>,
    callers: Option<Arc<dyn CallerFinder>>,
    feedback: Option<Arc<dyn FeedbackPolicy>>,
    human: Option<Arc<dyn HumanVerifications>>,
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

#[derive(Default)]
struct AuditSpend {
    performed: bool,
    usage: TokenUsage,
    cost_usd: Option<f64>,
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
        Ok(Self {
            source,
            reviewer,
            checks,
            settings,
            ignore,
            auditor: None,
            callers: None,
            feedback: None,
            human: None,
        })
    }

    pub fn with_auditor(mut self, auditor: Arc<dyn FindingAuditor>) -> Self {
        self.auditor = Some(auditor);
        self
    }

    #[must_use]
    pub fn with_callers(mut self, finder: Arc<dyn CallerFinder>) -> Self {
        self.callers = Some(finder);
        self
    }

    #[must_use]
    pub fn with_feedback(mut self, policy: Arc<dyn FeedbackPolicy>) -> Self {
        self.feedback = Some(policy);
        self
    }

    #[must_use]
    pub fn with_human_verifications(mut self, human: Arc<dyn HumanVerifications>) -> Self {
        self.human = Some(human);
        self
    }

    async fn human_inputs(&self, requirements: Option<&Requirements>) -> Vec<HumanInput> {
        match (&self.human, requirements) {
            (Some(human), Some(requirements)) => human.lookup(&requirements.criteria).await,
            _ => Vec::new(),
        }
    }

    async fn audit(&self, change_set: &ChangeSet, verification: Verification) -> (Verification, AuditSpend) {
        let Some(auditor) = &self.auditor else {
            return (verification, AuditSpend::default());
        };
        if verification.accepted.is_empty() {
            return (verification, AuditSpend::default());
        }
        let input = AuditInput { files: &change_set.files, findings: &verification.accepted };
        match auditor.audit(input).await {
            Ok(output) => {
                let spend = AuditSpend { performed: true, usage: output.usage, cost_usd: output.cost_usd };
                (apply_audit(change_set, verification, output.audits), spend)
            }
            Err(_) => (verification, AuditSpend::default()),
        }
    }

    fn effective_gate(&self) -> GatePolicy {
        if self.settings.block_acceptance {
            self.settings.gate.clone()
        } else {
            GatePolicy { fail_on_unmet_criteria: false, ..self.settings.gate.clone() }
        }
    }

    pub async fn execute(
        &self,
        requirements: Option<Requirements>,
        progress: &dyn ReviewProgress,
    ) -> Result<ReviewReport, ReviewError> {
        self.execute_with_agents(requirements, None, None, progress).await
    }

    pub async fn execute_with_agents(
        &self,
        requirements: Option<Requirements>,
        ticket: Option<TicketContext>,
        agents: Option<AgentRun<'_>>,
        progress: &dyn ReviewProgress,
    ) -> Result<ReviewReport, ReviewError> {
        let changes = self.source.load().await?;
        let (reviewable, mut skipped) = self.select(changes.files);
        let human = self.human_inputs(requirements.as_ref()).await;
        if reviewable.is_empty() {
            let mut report = self.empty_report(skipped, requirements, &human);
            report.ticket = ticket;
            return Ok(report);
        }
        let batches = plan_batches(&reviewable, self.settings.max_batch_weight, self.settings.max_files_per_batch);
        progress.started(reviewable.len(), batches.len());
        let callers = match &self.callers {
            Some(finder) => finder.find(&reviewable).await,
            None => CallerContext::default(),
        };
        let batch_callers: Vec<Vec<CallerSite>> = batches
            .iter()
            .map(|range| callers.for_files(reviewable[range.clone()].iter().map(|f| f.path.as_str())))
            .collect();

        let review = stream::iter(batches.into_iter().enumerate())
            .map(|(index, range)| self.review_batch(&reviewable[range], &batch_callers[index]))
            .buffer_unordered(self.settings.concurrency.max(1))
            .fold(BatchOutcome::default(), |acc, next| async move { acc.merge(next) });
        let clarifications = clarifications_of(requirements.as_ref(), &human);
        let assessment = self.assess(&reviewable, requirements.as_ref(), &clarifications);
        let agent_work = async {
            match &agents {
                Some(run) if !run.suite.is_empty() => {
                    Some(run_agents(run, &reviewable, requirements.as_ref(), &callers.sites).await)
                }
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
        let (mut verification, audit) = self.audit(&change_set, verification).await;
        let policy = self.effective_gate();
        if policy.require_reproduce_for_critical {
            verification.accepted = demote_unproven(verification.accepted);
        }
        if let Some(feedback) = &self.feedback {
            let assessed = feedback.assess(&verification.accepted).await;
            verification.accepted = apply_feedback(verification.accepted, &assessed);
        }
        let mut notes = Vec::new();
        let mut assessment_usage = assessment.usage;
        let mut assessment_cost = assessment.cost_usd;
        let mut requirements_report = requirements.map(|r| verify_assessments(&change_set, r, assessment.assessments));
        if let Some(report) = requirements_report.take() {
            let retry = retry_unverified(&self.reviewer, &change_set, report, &clarifications).await?;
            assessment_usage += retry.usage;
            assessment_cost = add_costs(assessment_cost, retry.cost_usd);
            notes.extend(retry.note);
            requirements_report = Some(apply_human_marks(retry.report, &human));
        }
        let incomplete = skipped.iter().any(|s| matches!(s.reason, SkipReason::ReviewFailed(_)));
        let agent_report = agent_report.map(|report| {
            let mut report = verify_agent_report(&change_set, report);
            report.notes.extend(callers.notes.iter().cloned());
            report
        });
        let gate = policy.decide(GateInput {
            findings: &verification.accepted,
            requirements: requirements_report.as_ref(),
            checks: &check_results,
            incomplete,
            agents: agent_report.as_ref(),
        });
        let mut usage = outcome.usage;
        usage += assessment_usage;
        usage += audit.usage;
        let unverifiable = collect_unverifiable(requirements_report.as_ref(), agent_report.as_ref(), &skipped);
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
            estimated_cost_usd: add_costs(add_costs(outcome.cost_usd, assessment_cost), audit.cost_usd),
            audited: audit.performed,
            notes,
            unverifiable,
        })
    }

    fn empty_report(
        &self,
        skipped: Vec<SkippedFile>,
        requirements: Option<Requirements>,
        human: &[HumanInput],
    ) -> ReviewReport {
        let requirements_report =
            requirements.map(|r| apply_human_marks(verify_assessments(&ChangeSet::default(), r, Vec::new()), human));
        let unverifiable = collect_unverifiable(requirements_report.as_ref(), None, &skipped);
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
            audited: false,
            notes: Vec::new(),
            unverifiable,
        }
    }

    async fn assess(
        &self,
        files: &[FileChange],
        requirements: Option<&Requirements>,
        clarifications: &[(String, String)],
    ) -> Result<AssessmentOutput, ReviewerError> {
        let Some(requirements) = requirements else {
            return Ok(AssessmentOutput::default());
        };
        let input = RequirementsInput { files, requirements, clarifications, expanded: false };
        match self.reviewer.assess_requirements(input).await {
            Ok(output) => Ok(output),
            Err(error) if error.is_fatal() => Err(error),
            Err(error) => Ok(AssessmentOutput {
                assessments: requirements
                    .criteria
                    .iter()
                    .map(|c| {
                        CriterionAssessment::unclear(
                            c.id.clone(),
                            UnverifiableReason::ToolError,
                            format!("Assessment failed: {error}"),
                        )
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

    fn review_batch<'a>(&'a self, files: &'a [FileChange], callers: &'a [CallerSite]) -> BoxFuture<'a, BatchOutcome> {
        Box::pin(async move {
            match self.attempt(files, callers).await {
                Ok(output) => BatchOutcome::success(output),
                Err(ReviewerError::Truncated) if files.len() > 1 => {
                    let (left, right) = files.split_at(files.len() / 2);
                    let (a, b) = futures::join!(self.review_batch(left, callers), self.review_batch(right, callers));
                    a.merge(b)
                }
                Err(error) if error.is_fatal() => BatchOutcome { fatal: Some(error), ..BatchOutcome::default() },
                Err(_) => match self.attempt(files, callers).await {
                    Ok(output) => BatchOutcome::success(output),
                    Err(error) if error.is_fatal() => BatchOutcome { fatal: Some(error), ..BatchOutcome::default() },
                    Err(error) => BatchOutcome::failed(files, &error),
                },
            }
        })
    }

    async fn attempt(&self, files: &[FileChange], callers: &[CallerSite]) -> Result<ReviewerOutput, ReviewerError> {
        self.reviewer.review(ReviewInput { files, guidelines: &self.settings.guidelines, callers }).await
    }
}

fn clarifications_of(requirements: Option<&Requirements>, human: &[HumanInput]) -> Vec<(String, String)> {
    let Some(requirements) = requirements else {
        return Vec::new();
    };
    requirements
        .criteria
        .iter()
        .zip(human)
        .filter_map(|(criterion, input)| {
            let text = input.clarification.as_deref()?.trim();
            (!text.is_empty()).then(|| (criterion.id.clone(), text.to_string()))
        })
        .collect()
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
    use crate::review::application::ports::AuditOutput;
    use crate::review::domain::change_set::fixtures::{added, file};
    use crate::review::domain::checks::{CheckOutcome, CheckResult};
    use crate::review::domain::feedback::{FeedbackMark, FindingFeedback};
    use crate::review::domain::finding::fixtures::finding;
    use crate::review::domain::finding::{Demotion, Reproduce, Severity};
    use crate::review::domain::gate::UnverifiablePolicy;
    use crate::review::domain::requirements::{AcceptanceCriterion, CriterionStatus, EvidenceRef, HumanMark};
    use crate::review::domain::unverifiable::UnverifiableSubject;
    use crate::review::domain::verification::{AuditVerdict, FindingAudit, RejectReason};

    struct FakeSource(ChangeSet);

    impl ChangeSource for FakeSource {
        async fn load(&self) -> Result<ChangeSet, SourceError> {
            Ok(self.0.clone())
        }
    }

    type AssessCall = (Vec<String>, bool, Vec<String>);

    struct FakeReviewer {
        script: Mutex<Vec<Result<ReviewerOutput, ReviewerError>>>,
        calls: Mutex<Vec<usize>>,
        callers_seen: Mutex<Vec<Vec<String>>>,
        assessment_script: Mutex<Vec<Option<ReviewerError>>>,
        assess_calls: Mutex<Vec<AssessCall>>,
    }

    impl FakeReviewer {
        fn new(mut script: Vec<Result<ReviewerOutput, ReviewerError>>) -> Self {
            script.reverse();
            Self {
                script: Mutex::new(script),
                calls: Mutex::new(Vec::new()),
                callers_seen: Mutex::new(Vec::new()),
                assessment_script: Mutex::new(Vec::new()),
                assess_calls: Mutex::new(Vec::new()),
            }
        }

        fn failing_assessments(self, mut script: Vec<Option<ReviewerError>>) -> Self {
            script.reverse();
            *self.assessment_script.lock().unwrap() = script;
            self
        }
    }

    impl CodeReviewer for FakeReviewer {
        async fn review(&self, input: ReviewInput<'_>) -> Result<ReviewerOutput, ReviewerError> {
            self.calls.lock().unwrap().push(input.files.len());
            self.callers_seen.lock().unwrap().push(input.callers.iter().map(|c| c.file.clone()).collect());
            self.script.lock().unwrap().pop().unwrap_or_else(|| Ok(ReviewerOutput::default()))
        }

        async fn assess_requirements(&self, input: RequirementsInput<'_>) -> Result<AssessmentOutput, ReviewerError> {
            let ids = input.requirements.criteria.iter().map(|c| c.id.clone()).collect();
            let notes = input.clarifications.iter().map(|(_, text)| text.clone()).collect();
            self.assess_calls.lock().unwrap().push((ids, input.expanded, notes));
            if let Some(error) = self.assessment_script.lock().unwrap().pop().flatten() {
                return Err(error);
            }
            let id = input.requirements.criteria[0].id.clone();
            Ok(AssessmentOutput {
                assessments: vec![CriterionAssessment {
                    criterion_id: id,
                    status: CriterionStatus::Met,
                    explanation: "done".into(),
                    evidence: vec![EvidenceRef { file: "a.go".into(), line: 1, quote: "total := compute(1)".into() }],
                    reason: None,
                    retried: false,
                    human: None,
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
            criteria: ids.iter().map(|id| AcceptanceCriterion::new(*id, "do it")).collect(),
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

    fn proven(mut critical: Finding) -> Finding {
        critical.reproduce = Reproduce::new("compute(1)", "returns 1", "returns 0", "");
        critical
    }

    #[tokio::test]
    async fn critical_without_reproduce_is_demoted_and_does_not_block_unless_policy_disables_it() {
        let critical = || finding("a.go", 1, Severity::Critical, "total := compute(1)");
        let reviewer = FakeReviewer::new(vec![Ok(output(vec![critical()], 0.0))]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await.unwrap();
        assert!(report.gate.passed);
        assert_eq!(report.findings[0].severity, Severity::Major);
        assert_eq!(report.findings[0].demoted, Some(Demotion::MissingReproduce));

        let mut lenient = sequential();
        lenient.gate.require_reproduce_for_critical = false;
        let reviewer = FakeReviewer::new(vec![Ok(output(vec![critical()], 0.0))]);
        let report = use_case(vec![go_file("a.go")], reviewer, lenient).execute(None, &NoProgress).await.unwrap();
        assert!(!report.gate.passed);
        assert_eq!(report.findings[0].demoted, None);
    }

    struct DismissFirst;

    impl FeedbackPolicy for DismissFirst {
        fn assess<'a>(&'a self, findings: &'a [Finding]) -> BoxFuture<'a, Vec<FindingFeedback>> {
            let count = findings.len();
            Box::pin(async move {
                (0..count)
                    .map(|i| FindingFeedback { dismissed: i == 0, cap: Some(Severity::Minor), score: Some(20) })
                    .collect()
            })
        }
    }

    #[tokio::test]
    async fn feedback_policy_dismisses_and_caps_accepted_findings() {
        let mut proven = finding("a.go", 1, Severity::Critical, "total := compute(1)");
        proven.reproduce = Reproduce::new("compute(1)", "returns 1", "returns 0", "");
        let other = finding("a.go", 2, Severity::Major, "total := compute(1)");
        let reviewer = FakeReviewer::new(vec![Ok(output(vec![proven, other], 0.0))]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential())
            .with_feedback(Arc::new(DismissFirst))
            .execute(None, &NoProgress)
            .await
            .unwrap();
        assert!(report.gate.passed);
        let dismissed = report.findings.iter().find(|f| f.feedback.is_some()).unwrap();
        assert_eq!(dismissed.feedback, Some(FeedbackMark::Dismissed));
        assert!(report.findings.iter().all(|f| f.severity == Severity::Minor));
        assert!(report.findings.iter().all(|f| matches!(f.demoted, Some(Demotion::LowPrecision { score: 20 }))));
    }

    struct FakeCallers;

    impl CallerFinder for FakeCallers {
        fn find<'a>(&'a self, files: &'a [FileChange]) -> BoxFuture<'a, CallerContext> {
            let site = |defined_in: &str, file: &str| CallerSite {
                symbol: "f".into(),
                defined_in: defined_in.into(),
                file: file.into(),
                line: 1,
                snippet: "f()".into(),
            };
            let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
            Box::pin(async move {
                assert_eq!(paths, ["a.go", "b.go"]);
                CallerContext { sites: vec![site("a.go", "api.go"), site("b.go", "job.go")], notes: vec!["n".into()] }
            })
        }
    }

    #[tokio::test]
    async fn each_batch_gets_only_the_callers_of_its_own_files() {
        let reviewer = FakeReviewer::new(vec![]);
        let settings = ReviewSettings { max_files_per_batch: 1, ..sequential() };
        let use_case =
            use_case(vec![go_file("a.go"), go_file("b.go")], reviewer, settings).with_callers(Arc::new(FakeCallers));
        use_case.execute(None, &NoProgress).await.unwrap();
        let mut seen = use_case.reviewer.callers_seen.lock().unwrap().clone();
        seen.sort();
        assert_eq!(seen, vec![vec!["api.go".to_string()], vec!["job.go".to_string()]]);
    }

    #[tokio::test]
    async fn verified_critical_finding_fails_gate() {
        let reviewer = FakeReviewer::new(vec![Ok(output(
            vec![
                proven(finding("a.go", 1, Severity::Critical, "total := compute(1)")),
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

    struct FakeAuditor(Result<AuditOutput, ReviewerError>);

    impl FindingAuditor for FakeAuditor {
        fn audit<'a>(&'a self, _input: AuditInput<'a>) -> BoxFuture<'a, Result<AuditOutput, ReviewerError>> {
            let result = match &self.0 {
                Ok(output) => Ok(output.clone()),
                Err(error) => Err(error.clone()),
            };
            Box::pin(async move { result })
        }
    }

    fn contradiction(index: usize) -> FindingAudit {
        FindingAudit {
            index,
            verdict: AuditVerdict::Contradicted,
            counter_file: "a.go".into(),
            counter_line: 1,
            counter_quote: "total := compute(1)".into(),
        }
    }

    fn audit_output(audits: Vec<FindingAudit>) -> AuditOutput {
        AuditOutput { audits, usage: TokenUsage { input_tokens: 3, ..TokenUsage::default() }, cost_usd: Some(0.02) }
    }

    fn audited_use_case(auditor: FakeAuditor) -> ReviewChanges<FakeSource, FakeReviewer, FakeChecks> {
        let reviewer = FakeReviewer::new(vec![Ok(output(
            vec![finding("a.go", 1, Severity::Critical, "total := compute(1)")],
            0.05,
        ))]);
        use_case(vec![go_file("a.go")], reviewer, sequential()).with_auditor(Arc::new(auditor))
    }

    #[tokio::test]
    async fn grounded_contradiction_rejects_the_finding_and_marks_report_audited() {
        let auditor = FakeAuditor(Ok(audit_output(vec![contradiction(0)])));
        let report = audited_use_case(auditor).execute(None, &NoProgress).await.unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(report.rejected[0].reason, RejectReason::ContradictedByCode);
        assert!(report.audited);
        assert!(report.gate.passed);
        assert_eq!(report.usage.input_tokens, 13);
        assert!((report.estimated_cost_usd.unwrap() - 0.07).abs() < 1e-9);
    }

    #[tokio::test]
    async fn failed_audit_keeps_findings_and_leaves_report_unaudited() {
        let auditor = FakeAuditor(Err(ReviewerError::Unavailable("down".into())));
        let report = audited_use_case(auditor).execute(None, &NoProgress).await.unwrap();
        assert_eq!(report.findings.len(), 1);
        assert!(!report.audited);
        assert_eq!(report.estimated_cost_usd, Some(0.05));
    }

    #[tokio::test]
    async fn auditor_is_skipped_when_no_finding_survives_verification() {
        let auditor = FakeAuditor(Err(ReviewerError::Rejected("must not be called".into())));
        let reviewer = FakeReviewer::new(vec![Ok(output(vec![], 0.05))]);
        let use_case = use_case(vec![go_file("a.go")], reviewer, sequential()).with_auditor(Arc::new(auditor));
        let report = use_case.execute(None, &NoProgress).await.unwrap();
        assert!(!report.audited);
    }

    #[tokio::test]
    async fn report_is_unaudited_without_an_auditor() {
        let reviewer = FakeReviewer::new(vec![Ok(output(
            vec![finding("a.go", 1, Severity::Critical, "total := compute(1)")],
            0.05,
        ))]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await.unwrap();
        assert!(!report.audited);
        assert_eq!(report.findings.len(), 1);
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
        let requirements = report.requirements.unwrap();
        assert_eq!(requirements.assessments[0].status, CriterionStatus::Unclear);
        assert!(!report.gate.passed, "an unaddressed acceptance criterion blocks under the strict policy");
        assert_eq!(report.gate.unmet_criteria, vec!["AC-1"]);
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
        let refused = || Err(ReviewerError::Refused { category: Some("cyber".into()) });
        let reviewer = FakeReviewer::new(vec![refused(), refused()]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await.unwrap();
        assert!(report.files_reviewed.is_empty());
        assert!(report.is_incomplete());
        assert!(report.gate.incomplete);
        assert!(!report.gate.passed, "an unreviewed change must not pass");
        assert_eq!(report.unverifiable.len(), 1);
        assert_eq!(report.unverifiable[0].subject, UnverifiableSubject::File);
        assert_eq!(report.unverifiable[0].id, "a.go");
    }

    #[tokio::test]
    async fn a_failed_batch_is_retried_once_and_recovers() {
        let reviewer =
            FakeReviewer::new(vec![Err(ReviewerError::Unavailable("blip".into())), Ok(output(vec![], 0.01))]);
        let use_case = use_case(vec![go_file("a.go")], reviewer, sequential());
        let report = use_case.execute(None, &NoProgress).await.unwrap();
        assert_eq!(*use_case.reviewer.calls.lock().unwrap(), vec![1, 1]);
        assert_eq!(report.files_reviewed, vec!["a.go"]);
        assert!(!report.is_incomplete());
        assert!(report.gate.passed);
        assert!(report.unverifiable.is_empty());
    }

    #[tokio::test]
    async fn a_fatal_error_on_the_batch_retry_aborts() {
        let reviewer = FakeReviewer::new(vec![
            Err(ReviewerError::Unavailable("blip".into())),
            Err(ReviewerError::RateLimited("limit".into())),
        ]);
        let result = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await;
        assert!(matches!(result, Err(ReviewError::Reviewer(ReviewerError::RateLimited(_)))));
    }

    #[tokio::test]
    async fn fatal_reviewer_error_aborts_review() {
        let reviewer = FakeReviewer::new(vec![Err(ReviewerError::Rejected("invalid x-api-key".into()))]);
        let result = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await;
        assert!(matches!(result, Err(ReviewError::Reviewer(ReviewerError::Rejected(_)))));
    }

    #[tokio::test]
    async fn a_usage_limit_stop_aborts_instead_of_passing_with_skipped_work() {
        let limited = ReviewerError::RateLimited("Claude usage limit reached (resets 5pm): x".into());
        let reviewer = FakeReviewer::new(vec![Err(limited)]);
        let result = use_case(vec![go_file("a.go")], reviewer, sequential()).execute(None, &NoProgress).await;
        assert!(matches!(result, Err(ReviewError::Reviewer(ReviewerError::RateLimited(_)))));
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
                            reason: None,
                            retried: false,
                            human: None,
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
    async fn unassessed_criterion_is_retried_with_full_files_and_resolved() {
        let use_case = use_case(vec![go_file("a.go")], FakeReviewer::new(vec![]), sequential());
        let report = use_case.execute(Some(requirements(&["AC-1", "AC-2"])), &NoProgress).await.unwrap();
        let req = report.requirements.as_ref().unwrap();
        assert_eq!(req.assessments[0].status, CriterionStatus::Met);
        assert!(!req.assessments[0].retried);
        assert_eq!(req.assessments[1].status, CriterionStatus::Met);
        assert!(req.assessments[1].retried);
        assert_eq!(report.usage.input_tokens, 14);
        assert!(report.gate.passed);
        assert!(report.unverifiable.is_empty());
        assert_eq!(report.notes, vec!["retried 1 unverifiable criteria with full-file context; 1 resolved"]);
        let calls = use_case.reviewer.assess_calls.lock().unwrap();
        assert_eq!(calls[0], (vec!["AC-1".to_string(), "AC-2".to_string()], false, vec![]));
        assert_eq!(calls[1], (vec!["AC-2".to_string()], true, vec![]));
    }

    #[tokio::test]
    async fn criterion_still_unclear_after_retry_is_reported_as_unverifiable() {
        let reviewer =
            FakeReviewer::new(vec![]).failing_assessments(vec![None, Some(ReviewerError::Unavailable("x".into()))]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential())
            .execute(Some(requirements(&["AC-1", "AC-2"])), &NoProgress)
            .await
            .unwrap();
        let req = report.requirements.as_ref().unwrap();
        assert_eq!(req.assessments[1].status, CriterionStatus::Unclear);
        assert!(!req.assessments[1].retried);
        assert!(!report.gate.passed, "an unclear acceptance criterion is not demonstrably met, so it blocks");
        assert_eq!(report.gate.unmet_criteria, vec!["AC-2"]);
        assert_eq!(report.gate.unverified_criteria, vec!["AC-2"]);
        assert_eq!(report.unverifiable.len(), 1);
        assert_eq!(report.unverifiable[0].reason, UnverifiableReason::NotAssessed);
        assert_eq!(report.unverifiable[0].subject, UnverifiableSubject::Criterion);
        assert!(report.notes[0].contains("failed"));
    }

    #[tokio::test]
    async fn advisory_policy_lists_unverified_criteria_without_blocking() {
        let reviewer =
            FakeReviewer::new(vec![]).failing_assessments(vec![None, Some(ReviewerError::Unavailable("x".into()))]);
        let mut settings = sequential();
        settings.gate.unverifiable = UnverifiablePolicy::Advisory;
        let report = use_case(vec![go_file("a.go")], reviewer, settings)
            .execute(Some(requirements(&["AC-1", "AC-2"])), &NoProgress)
            .await
            .unwrap();
        assert!(report.gate.passed);
        assert!(report.gate.unmet_criteria.is_empty());
        assert_eq!(report.gate.unverified_criteria, vec!["AC-2"]);
    }

    struct FakeHuman(Vec<HumanInput>);

    impl HumanVerifications for FakeHuman {
        fn lookup<'a>(&'a self, criteria: &'a [AcceptanceCriterion]) -> BoxFuture<'a, Vec<HumanInput>> {
            assert_eq!(criteria.len(), self.0.len());
            let inputs = self.0.clone();
            Box::pin(async move { inputs })
        }
    }

    #[tokio::test]
    async fn human_marks_and_clarifications_are_applied_around_the_assessment() {
        let reviewer =
            FakeReviewer::new(vec![]).failing_assessments(vec![None, Some(ReviewerError::Unavailable("x".into()))]);
        let human = FakeHuman(vec![
            HumanInput { mark: None, clarification: Some("  CSV only  ".into()) },
            HumanInput { mark: Some(HumanMark::VerifiedMet), clarification: Some("   ".into()) },
        ]);
        let use_case =
            use_case(vec![go_file("a.go")], reviewer, sequential()).with_human_verifications(Arc::new(human));
        let report = use_case.execute(Some(requirements(&["AC-1", "AC-2"])), &NoProgress).await.unwrap();
        let req = report.requirements.as_ref().unwrap();
        assert_eq!(req.assessments[1].status, CriterionStatus::Met);
        assert_eq!(req.assessments[1].human, Some(HumanMark::VerifiedMet));
        assert!(report.gate.passed);
        assert!(report.unverifiable.is_empty());
        assert_eq!(use_case.reviewer.assess_calls.lock().unwrap()[0].2, vec!["CSV only".to_string()]);
    }

    #[tokio::test]
    async fn not_applicable_mark_excludes_the_criterion_from_the_gate_and_unverifiable_list() {
        let reviewer =
            FakeReviewer::new(vec![]).failing_assessments(vec![None, Some(ReviewerError::Unavailable("x".into()))]);
        let human = FakeHuman(vec![
            HumanInput::default(),
            HumanInput { mark: Some(HumanMark::NotApplicable), clarification: None },
        ]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential())
            .with_human_verifications(Arc::new(human))
            .execute(Some(requirements(&["AC-1", "AC-2"])), &NoProgress)
            .await
            .unwrap();
        assert!(report.gate.passed);
        assert!(report.gate.unverified_criteria.is_empty());
        assert!(report.unverifiable.is_empty());
    }

    #[tokio::test]
    async fn empty_change_applies_human_marks_and_lists_unverifiable_criteria() {
        let human = FakeHuman(vec![
            HumanInput { mark: Some(HumanMark::NotApplicable), clarification: None },
            HumanInput::default(),
        ]);
        let report = use_case(vec![], FakeReviewer::new(vec![]), sequential())
            .with_human_verifications(Arc::new(human))
            .execute(Some(requirements(&["AC-1", "AC-2"])), &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.unverifiable.len(), 1);
        assert_eq!(report.unverifiable[0].id, "AC-2");
        assert_eq!(report.unverifiable[0].reason, UnverifiableReason::NotAssessed);
    }

    #[tokio::test]
    async fn failed_assessment_marks_criteria_unclear_but_fatal_aborts() {
        let reviewer = FakeReviewer::new(vec![]).failing_assessments(vec![
            Some(ReviewerError::Unavailable("overloaded".into())),
            Some(ReviewerError::Unavailable("overloaded".into())),
        ]);
        let report = use_case(vec![go_file("a.go")], reviewer, sequential())
            .execute(Some(requirements(&["AC-1"])), &NoProgress)
            .await
            .unwrap();
        let assessment = &report.requirements.unwrap().assessments[0];
        assert_eq!(assessment.status, CriterionStatus::Unclear);
        assert_eq!(assessment.reason, Some(UnverifiableReason::ToolError));
        assert!(assessment.explanation.contains("overloaded"));

        let reviewer = FakeReviewer::new(vec![]).failing_assessments(vec![Some(ReviewerError::Rejected("401".into()))]);
        let result = use_case(vec![go_file("a.go")], reviewer, sequential())
            .execute(Some(requirements(&["AC-1"])), &NoProgress)
            .await;
        assert!(result.is_err());

        let reviewer = FakeReviewer::new(vec![]).failing_assessments(vec![
            Some(ReviewerError::Unavailable("x".into())),
            Some(ReviewerError::RateLimited("l".into())),
        ]);
        let result = use_case(vec![go_file("a.go")], reviewer, sequential())
            .execute(Some(requirements(&["AC-1"])), &NoProgress)
            .await;
        assert!(matches!(result, Err(ReviewError::Reviewer(ReviewerError::RateLimited(_)))));
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
