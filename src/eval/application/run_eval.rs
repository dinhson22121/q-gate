use std::sync::Arc;
use std::time::Instant;

use super::ports::{CaseRepository, CaseWorkspace, Clock, EvalError, EvalStore, LoadedCase, ReviewJob, ReviewRunner};
use crate::eval::domain::case::criterion_id;
use crate::eval::domain::run::EvalRun;
use crate::eval::domain::scoring::{CaseScore, ObservedFinding, RunMeasure, score_case};
use crate::eval::domain::test_outcome::TestsOutcome;
use crate::history::application::RunProvenance;
use crate::review::application::{CriterionStatus, ReviewReport, TicketContext};

pub const MAX_RUNS: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOptions {
    pub only: Vec<String>,
    pub runs: u32,
    pub with_tests: bool,
}

impl Default for EvalOptions {
    fn default() -> Self {
        Self { only: Vec::new(), runs: 1, with_tests: false }
    }
}

pub trait EvalProgress: Send + Sync {
    fn scored(&self, score: &CaseScore, done: usize, total: usize);
}

pub struct NoEvalProgress;

impl EvalProgress for NoEvalProgress {
    fn scored(&self, _score: &CaseScore, _done: usize, _total: usize) {}
}

pub struct RunEval {
    cases: Arc<dyn CaseRepository>,
    workspace: Arc<dyn CaseWorkspace>,
    runner: Arc<dyn ReviewRunner>,
    store: Arc<dyn EvalStore>,
    clock: Clock,
}

impl RunEval {
    pub fn new(
        cases: Arc<dyn CaseRepository>,
        workspace: Arc<dyn CaseWorkspace>,
        runner: Arc<dyn ReviewRunner>,
        store: Arc<dyn EvalStore>,
        clock: Clock,
    ) -> Self {
        Self { cases, workspace, runner, store, clock }
    }

    pub async fn execute(
        &self,
        options: &EvalOptions,
        provenance: RunProvenance,
        progress: &dyn EvalProgress,
    ) -> Result<EvalRun, EvalError> {
        if !(1..=MAX_RUNS).contains(&options.runs) {
            return Err(EvalError::InvalidInput(format!("runs must be between 1 and {MAX_RUNS}")));
        }
        let selected = self.load_selected(&options.only)?;
        let total = selected.len() * options.runs as usize;
        let mut results = Vec::with_capacity(total);
        for loaded in &selected {
            let repo = self.workspace.materialize(loaded).await?;
            for repetition in 0..options.runs {
                let job = review_job(loaded, &repo.root, &repo.base, &repo.head, options.with_tests);
                let started = Instant::now();
                let report = self.runner.run(&job).await.map_err(|e| EvalError::Review {
                    case_id: loaded.case.id.clone(),
                    repetition,
                    message: e.to_string(),
                })?;
                let measure = RunMeasure {
                    repetition,
                    duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    cost_usd: report.estimated_cost_usd.unwrap_or(0.0),
                };
                let score = score_report(loaded, &report, measure, options.with_tests);
                progress.scored(&score, results.len() + 1, total);
                results.push(score);
            }
        }
        let mut run = EvalRun::new((self.clock)(), provenance, options.runs, options.with_tests, results);
        run.id = self.store.save(&run)?;
        Ok(run)
    }

    fn load_selected(&self, only: &[String]) -> Result<Vec<LoadedCase>, EvalError> {
        let available = self.cases.ids()?;
        if let Some(missing) = only.iter().find(|id| !available.contains(id)) {
            return Err(EvalError::NotFound(format!("no case '{missing}' (available: {})", available.join(", "))));
        }
        let ids: Vec<&String> = available.iter().filter(|id| only.is_empty() || only.contains(id)).collect();
        if ids.is_empty() {
            return Err(EvalError::NotFound("no golden cases found".into()));
        }
        ids.into_iter().map(|id| self.cases.load(id)).collect()
    }
}

fn review_job(loaded: &LoadedCase, root: &std::path::Path, base: &str, head: &str, with_tests: bool) -> ReviewJob {
    let ticket = loaded.case.ticket.as_ref().map(|ticket| TicketContext {
        keys: vec![ticket.key.clone()],
        description: ticket.description.clone(),
        ..TicketContext::default()
    });
    ReviewJob {
        repo: root.to_path_buf(),
        base: base.to_string(),
        head: head.to_string(),
        requirements: loaded.case.requirements(),
        ticket,
        with_tests,
    }
}

fn score_report(loaded: &LoadedCase, report: &ReviewReport, measure: RunMeasure, with_tests: bool) -> CaseScore {
    let findings: Vec<ObservedFinding> = report
        .findings
        .iter()
        .map(|f| ObservedFinding {
            file: f.file.clone(),
            line: f.line,
            category: f.category,
            severity: f.severity,
            title: f.title.clone(),
            explanation: f.explanation.clone(),
        })
        .collect();
    let statuses: Vec<Option<CriterionStatus>> = match (&loaded.case.ticket, &report.requirements) {
        (Some(ticket), Some(requirements)) => (0..loaded.case.criteria.len())
            .map(|index| {
                let id = criterion_id(&ticket.key, index);
                requirements.assessments.iter().find(|a| a.criterion_id == id).map(|a| a.status)
            })
            .collect(),
        _ => Vec::new(),
    };
    CaseScore {
        content_hash: loaded.content_hash.clone(),
        tests: with_tests.then(|| TestsOutcome::from_report(report.agents.as_ref())),
        ..score_case(&loaded.case, &findings, &statuses, measure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::application::test_support::{
        FakeCases, FakeWorkspace, MemoryStore, ScriptedRunner, fixed_clock, provenance, report_with,
    };
    use crate::review::application::{Category, Severity};

    const SECURITY_AT_12: &str =
        "id = \"a-case\"\nlanguage = \"go\"\n[[expected]]\nfile = \"a.go\"\nline = 12\ncategory = \"security\"\n";

    struct Fixture {
        cases: Arc<FakeCases>,
        workspace: Arc<FakeWorkspace>,
        runner: Arc<ScriptedRunner>,
        store: Arc<MemoryStore>,
    }

    impl Fixture {
        fn new(cases: &[(&str, &str)], reports: Vec<Result<ReviewReport, EvalError>>) -> Self {
            Self {
                cases: Arc::new(FakeCases::new(cases)),
                workspace: Arc::new(FakeWorkspace::default()),
                runner: Arc::new(ScriptedRunner::new(reports)),
                store: Arc::new(MemoryStore::default()),
            }
        }

        fn use_case(&self) -> RunEval {
            RunEval::new(
                self.cases.clone(),
                self.workspace.clone(),
                self.runner.clone(),
                self.store.clone(),
                fixed_clock(),
            )
        }
    }

    fn catching() -> ReviewReport {
        report_with(&[("a.go", 12, Category::Security, Severity::Major)], &[], 0.4)
    }

    #[tokio::test]
    async fn scores_every_case_repetition_and_stores_the_run() {
        let fixture = Fixture::new(&[("a-case", SECURITY_AT_12)], vec![Ok(catching()), Ok(report_with(&[], &[], 0.1))]);
        let options = EvalOptions { runs: 2, ..EvalOptions::default() };

        let run = fixture.use_case().execute(&options, provenance(), &NoEvalProgress).await.unwrap();

        assert_eq!(run.id, 1);
        assert_eq!(run.results.len(), 2);
        assert!(run.results[0].expected[0].caught && !run.results[1].expected[0].caught);
        assert_eq!(run.metrics.recall.mean, Some(0.5));
        assert!((run.metrics.cost_usd - 0.5).abs() < 1e-9);
        assert_eq!(fixture.store.get(1).unwrap().unwrap(), run);
        assert_eq!(fixture.workspace.materialized(), 1, "one workspace per case, shared by its repetitions");
    }

    #[tokio::test]
    async fn with_tests_records_the_test_agent_outcome_per_result() {
        let mut ran = catching();
        ran.agents = Some(crate::review::application::AgentReport {
            tests: vec![crate::review::application::TestResult {
                scenario_id: "S1".into(),
                language: "go".into(),
                verdict: crate::review::application::TestVerdict::Passed,
                evidence: None,
                note: String::new(),
                reason: None,
            }],
            cost_usd: Some(0.2),
            ..crate::review::application::AgentReport::default()
        });
        let fixture = Fixture::new(&[("a-case", SECURITY_AT_12)], vec![Ok(ran), Ok(catching())]);
        let options = EvalOptions { with_tests: true, runs: 2, ..EvalOptions::default() };

        let run = fixture.use_case().execute(&options, provenance(), &NoEvalProgress).await.unwrap();

        let first = run.results[0].tests.as_ref().unwrap();
        assert!(first.ran && first.passed == 1 && (first.cost_usd - 0.2).abs() < 1e-9);
        let second = run.results[1].tests.as_ref().unwrap();
        assert!(!second.ran && second.notes == ["no agent report was produced"]);
    }

    #[tokio::test]
    async fn without_tests_no_outcome_is_recorded() {
        let fixture = Fixture::new(&[("a-case", SECURITY_AT_12)], vec![Ok(catching())]);

        let run = fixture.use_case().execute(&EvalOptions::default(), provenance(), &NoEvalProgress).await.unwrap();

        assert!(run.results[0].tests.is_none());
    }

    #[tokio::test]
    async fn the_review_job_carries_range_requirements_and_flags() {
        let toml = format!(
            "{SECURITY_AT_12}[ticket]\nkey = \"KAN-1\"\ntitle = \"T\"\ndescription = \"D\"\n[[ac]]\ntext = \"works\"\nexpect = \"met\"\n"
        );
        let fixture = Fixture::new(&[("a-case", toml.as_str())], vec![Ok(catching())]);
        let options = EvalOptions { with_tests: true, ..EvalOptions::default() };

        fixture.use_case().execute(&options, provenance(), &NoEvalProgress).await.unwrap();

        let job = fixture.runner.jobs().remove(0);
        assert!(job.with_tests);
        assert_eq!((job.base.as_str(), job.head.as_str()), ("base-sha", "head-sha"));
        assert_eq!(job.requirements.unwrap().criteria[0].id, "KAN-1/AC-1");
        assert_eq!(job.ticket.unwrap().description, "D");
    }

    #[tokio::test]
    async fn criteria_statuses_come_from_the_requirements_report() {
        let toml = "id = \"a-case\"\nlanguage = \"go\"\n[ticket]\nkey = \"KAN-1\"\n[[ac]]\ntext = \"one\"\nexpect = \"met\"\n[[ac]]\ntext = \"two\"\nexpect = \"not_met\"\n";
        let report = report_with(&[], &[("KAN-1/AC-1", CriterionStatus::Met)], 0.0);
        let fixture = Fixture::new(&[("a-case", toml)], vec![Ok(report)]);

        let run = fixture.use_case().execute(&EvalOptions::default(), provenance(), &NoEvalProgress).await.unwrap();

        let verdicts: Vec<_> = run.results[0].criteria.iter().map(|c| c.verdict).collect();
        use crate::eval::domain::scoring::CriterionVerdict::{Correct, Unclear};
        assert_eq!(verdicts, [Correct, Unclear], "a missing assessment is unclear, never silently correct");
    }

    #[tokio::test]
    async fn only_filters_cases_and_unknown_ids_are_reported() {
        let b = SECURITY_AT_12.replace("a-case", "b-case");
        let fixture = Fixture::new(&[("a-case", SECURITY_AT_12), ("b-case", b.as_str())], vec![Ok(catching())]);
        let only = |ids: &[&str]| EvalOptions {
            only: ids.iter().map(ToString::to_string).collect(),
            ..EvalOptions::default()
        };

        let run = fixture.use_case().execute(&only(&["b-case"]), provenance(), &NoEvalProgress).await.unwrap();
        let error = fixture.use_case().execute(&only(&["zzz"]), provenance(), &NoEvalProgress).await.unwrap_err();

        assert_eq!(run.results[0].case_id, "b-case");
        assert!(matches!(&error, EvalError::NotFound(m) if m.contains("zzz") && m.contains("a-case")));
    }

    #[tokio::test]
    async fn invalid_runs_and_empty_case_sets_are_rejected() {
        let fixture = Fixture::new(&[], vec![]);
        let use_case = fixture.use_case();

        for runs in [0, MAX_RUNS + 1] {
            let options = EvalOptions { runs, ..EvalOptions::default() };
            let error = use_case.execute(&options, provenance(), &NoEvalProgress).await.unwrap_err();
            assert!(matches!(error, EvalError::InvalidInput(_)), "{runs}");
        }
        let error = use_case.execute(&EvalOptions::default(), provenance(), &NoEvalProgress).await.unwrap_err();
        assert!(matches!(error, EvalError::NotFound(_)));
    }

    #[tokio::test]
    async fn a_failed_review_aborts_without_storing_a_partial_run() {
        let failure = EvalError::Storage("boom".into());
        let fixture = Fixture::new(&[("a-case", SECURITY_AT_12)], vec![Err(failure)]);

        let error =
            fixture.use_case().execute(&EvalOptions::default(), provenance(), &NoEvalProgress).await.unwrap_err();

        assert!(matches!(&error, EvalError::Review { case_id, repetition: 0, .. } if case_id == "a-case"));
        assert!(fixture.store.latest().unwrap().is_none());
    }

    #[tokio::test]
    async fn progress_sees_each_scored_repetition() {
        struct Recorder(std::sync::Mutex<Vec<(usize, usize)>>);
        impl EvalProgress for Recorder {
            fn scored(&self, _score: &CaseScore, done: usize, total: usize) {
                self.0.lock().unwrap().push((done, total));
            }
        }
        let fixture = Fixture::new(&[("a-case", SECURITY_AT_12)], vec![Ok(catching()), Ok(catching())]);
        let recorder = Recorder(Default::default());
        let options = EvalOptions { runs: 2, ..EvalOptions::default() };

        fixture.use_case().execute(&options, provenance(), &recorder).await.unwrap();

        assert_eq!(*recorder.0.lock().unwrap(), [(1, 2), (2, 2)]);
    }
}
