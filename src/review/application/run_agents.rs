use std::path::Path;
use std::sync::Arc;

use futures::{StreamExt, stream};

use super::agent_ports::{AgentInput, ArchitectureReviewer, DesignReviewer, TestAgent, TicketAnalyst};
use crate::review::domain::agent_results::{AgentReport, TicketBrief, TicketContext};
use crate::review::domain::callers::CallerSite;
use crate::review::domain::change_set::FileChange;
use crate::review::domain::requirements::Requirements;

#[derive(Clone, Default)]
pub struct AgentSuite {
    pub analyst: Option<Arc<dyn TicketAnalyst>>,
    pub tests: Option<Arc<dyn TestAgent>>,
    pub architecture: Option<Arc<dyn ArchitectureReviewer>>,
    pub design: Option<Arc<dyn DesignReviewer>>,
    pub max_parallel: usize,
    pub max_scenarios: usize,
}

impl AgentSuite {
    pub fn is_empty(&self) -> bool {
        self.analyst.is_none() && self.tests.is_none() && self.architecture.is_none() && self.design.is_none()
    }
}

pub struct AgentRun<'a> {
    pub suite: &'a AgentSuite,
    pub ticket: Option<&'a TicketContext>,
    pub commit: &'a str,
    pub repo_root: &'a Path,
    pub snapshot_root: Option<&'a Path>,
}

pub async fn run_agents(
    run: &AgentRun<'_>,
    files: &[FileChange],
    requirements: Option<&Requirements>,
    callers: &[CallerSite],
) -> AgentReport {
    let input = AgentInput {
        files,
        requirements,
        ticket: run.ticket,
        commit: run.commit,
        repo_root: run.repo_root,
        snapshot_root: run.snapshot_root,
        callers,
    };
    let suite = run.suite;
    let mut notes = Vec::new();
    let brief = analyze(suite, &input, &mut notes).await;

    let tests = run_tests(suite, &input, brief.as_ref(), &mut notes);
    let architecture = async {
        match &suite.architecture {
            Some(reviewer) => Some(reviewer.review(&input, brief.as_ref()).await),
            None => None,
        }
    };
    let design = async {
        match &suite.design {
            Some(reviewer) => Some(reviewer.review(&input, brief.as_ref()).await),
            None => None,
        }
    };
    let (mut tests, architecture, design) = futures::join!(tests, architecture, design);
    tests.sort_by_key(|t| natural_key(&t.scenario_id));

    let (violations, rule_assessments) = match architecture {
        Some(Ok(review)) => (review.violations, review.assessments),
        Some(Err(e)) => {
            notes.push(format!("architecture review failed: {e}"));
            (Vec::new(), Vec::new())
        }
        None => (Vec::new(), Vec::new()),
    };
    let design = match design {
        Some(Ok(mismatches)) => mismatches,
        Some(Err(e)) => {
            notes.push(format!("design review skipped: {e}"));
            Vec::new()
        }
        None => Vec::new(),
    };
    AgentReport { brief, tests, violations, rule_assessments, design, notes, cost_usd: None }
}

async fn analyze(suite: &AgentSuite, input: &AgentInput<'_>, notes: &mut Vec<String>) -> Option<TicketBrief> {
    let analyst = suite.analyst.as_ref()?;
    match input.requirements {
        Some(r) if !r.criteria.is_empty() => match analyst.analyze(input).await {
            Ok(brief) => Some(brief),
            Err(e) => {
                notes.push(format!("ticket analysis failed: {e}"));
                None
            }
        },
        _ => {
            notes.push("ticket analysis skipped: no acceptance criteria for this change".into());
            None
        }
    }
}

fn run_tests<'a>(
    suite: &'a AgentSuite,
    input: &'a AgentInput<'a>,
    brief: Option<&'a TicketBrief>,
    notes: &mut Vec<String>,
) -> impl Future<Output = Vec<crate::review::domain::agent_results::TestResult>> + 'a {
    let scenarios: Vec<_> = match (&suite.tests, brief) {
        (Some(_), Some(brief)) => brief.scenarios.iter().take(suite.max_scenarios.max(1)).collect(),
        (Some(_), None) => {
            notes.push("test agents skipped: no scenarios (ticket analysis did not run)".into());
            Vec::new()
        }
        _ => Vec::new(),
    };
    if let Some(brief) = brief.filter(|b| b.scenarios.len() > suite.max_scenarios.max(1) && suite.tests.is_some()) {
        notes.push(format!(
            "only the first {} of {} scenarios were tested (agents.tests.max_scenarios)",
            suite.max_scenarios,
            brief.scenarios.len()
        ));
    }
    let agent = suite.tests.clone();
    let parallel = suite.max_parallel.max(1);
    async move {
        let Some(agent) = agent else { return Vec::new() };
        let runs: Vec<_> = scenarios.into_iter().map(|scenario| agent.run(input, scenario)).collect();
        stream::iter(runs).buffer_unordered(parallel).collect().await
    }
}

fn natural_key(id: &str) -> (String, u64) {
    let digits: String =
        id.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    let prefix = id[..id.len() - digits.len()].to_string();
    (prefix, digits.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::BoxFuture;

    use super::*;
    use crate::review::application::agent_ports::{AgentError, ArchitectureReview};
    use crate::review::domain::agent_results::*;
    use crate::review::domain::requirements::AcceptanceCriterion;

    fn scenario(id: &str) -> TestScenario {
        TestScenario {
            id: id.into(),
            criterion_ids: vec!["K-1/AC-1".into()],
            title: id.into(),
            given: "g".into(),
            when: "w".into(),
            then: "t".into(),
        }
    }

    struct Analyst(usize);
    impl TicketAnalyst for Analyst {
        fn analyze<'a>(&'a self, _input: &'a AgentInput<'a>) -> BoxFuture<'a, Result<TicketBrief, AgentError>> {
            let scenarios = (1..=self.0).map(|i| scenario(&format!("S{i}"))).collect();
            Box::pin(async move { Ok(TicketBrief { scenarios, ..TicketBrief::default() }) })
        }
    }

    struct FailingAnalyst;
    impl TicketAnalyst for FailingAnalyst {
        fn analyze<'a>(&'a self, _input: &'a AgentInput<'a>) -> BoxFuture<'a, Result<TicketBrief, AgentError>> {
            Box::pin(async { Err(AgentError("Claude unavailable".into())) })
        }
    }

    #[derive(Default)]
    struct Tests {
        running: AtomicUsize,
        max_seen: AtomicUsize,
        seen: Mutex<Vec<String>>,
    }
    impl TestAgent for Tests {
        fn run<'a>(&'a self, _input: &'a AgentInput<'a>, scenario: &'a TestScenario) -> BoxFuture<'a, TestResult> {
            Box::pin(async move {
                let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_seen.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                self.running.fetch_sub(1, Ordering::SeqCst);
                self.seen.lock().unwrap().push(scenario.id.clone());
                TestResult {
                    scenario_id: scenario.id.clone(),
                    language: "go".into(),
                    verdict: TestVerdict::Passed,
                    evidence: None,
                    note: String::new(),
                    reason: None,
                }
            })
        }
    }

    struct Arch(Result<Vec<RuleViolation>, AgentError>);
    impl ArchitectureReviewer for Arch {
        fn review<'a>(
            &'a self,
            _input: &'a AgentInput<'a>,
            brief: Option<&'a TicketBrief>,
        ) -> BoxFuture<'a, Result<ArchitectureReview, AgentError>> {
            assert!(brief.is_some(), "architecture sees the brief");
            let result = self.0.clone().map(|violations| ArchitectureReview { violations, assessments: Vec::new() });
            Box::pin(async move { result })
        }
    }

    struct Design;
    impl DesignReviewer for Design {
        fn review<'a>(
            &'a self,
            _input: &'a AgentInput<'a>,
            _brief: Option<&'a TicketBrief>,
        ) -> BoxFuture<'a, Result<Vec<DesignMismatch>, AgentError>> {
            Box::pin(async { Err(AgentError("no Figma link on the ticket or its Story".into())) })
        }
    }

    fn requirements() -> Requirements {
        Requirements {
            key: "K-1".into(),
            title: "t".into(),
            url: None,
            criteria: vec![AcceptanceCriterion::new("K-1/AC-1", "works")],
        }
    }

    async fn run(suite: &AgentSuite, requirements: Option<&Requirements>) -> AgentReport {
        let run = AgentRun { suite, ticket: None, commit: "abc", repo_root: Path::new("/repo"), snapshot_root: None };
        run_agents(&run, &[], requirements, &[]).await
    }

    #[tokio::test]
    async fn analyst_feeds_bounded_parallel_tests_and_failures_become_notes() {
        let tests = Arc::new(Tests::default());
        let suite = AgentSuite {
            analyst: Some(Arc::new(Analyst(12))),
            tests: Some(tests.clone()),
            architecture: Some(Arc::new(Arch(Err(AgentError("rulebook build failed".into()))))),
            design: Some(Arc::new(Design)),
            max_parallel: 2,
            max_scenarios: 10,
        };
        let report = run(&suite, Some(&requirements())).await;
        let ids: Vec<_> = report.tests.iter().map(|t| t.scenario_id.as_str()).collect();
        assert_eq!(ids, vec!["S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9", "S10"]);
        assert!(tests.max_seen.load(Ordering::SeqCst) <= 2);
        assert!(report.notes.iter().any(|n| n.contains("first 10 of 12")));
        assert!(report.notes.iter().any(|n| n.contains("rulebook build failed")));
        assert!(report.notes.iter().any(|n| n.contains("no Figma link")));
        assert_eq!(report.brief.unwrap().scenarios.len(), 12);
    }

    #[tokio::test]
    async fn without_criteria_or_with_failed_analysis_tests_are_skipped_with_a_note() {
        let suite = AgentSuite {
            analyst: Some(Arc::new(Analyst(3))),
            tests: Some(Arc::new(Tests::default())),
            max_parallel: 1,
            max_scenarios: 5,
            ..AgentSuite::default()
        };
        let report = run(&suite, None).await;
        assert!(report.tests.is_empty());
        assert!(report.notes.iter().any(|n| n.contains("no acceptance criteria")));

        let failing = AgentSuite { analyst: Some(Arc::new(FailingAnalyst)), ..suite };
        let report = run(&failing, Some(&requirements())).await;
        assert!(report.notes.iter().any(|n| n.contains("Claude unavailable")));
        assert!(report.notes.iter().any(|n| n.contains("test agents skipped")));
    }

    #[test]
    fn empty_suite_and_natural_order() {
        assert!(AgentSuite::default().is_empty());
        assert!(natural_key("S2") < natural_key("S10"));
    }
}
