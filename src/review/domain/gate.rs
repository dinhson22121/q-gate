use serde::{Deserialize, Serialize};

use super::agent_results::{AgentReport, DesignMismatchKind, TestVerdict};
use super::checks::CheckResult;
use super::feedback::FeedbackMark;
use super::finding::{Category, Confidence, Finding, Severity};
use super::requirements::RequirementsReport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnverifiablePolicy {
    #[default]
    Strict,
    Advisory,
}

impl UnverifiablePolicy {
    pub const ALL: [Self; 2] = [Self::Strict, Self::Advisory];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Advisory => "advisory",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == value.trim().to_ascii_lowercase())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatePolicy {
    pub fail_on: Severity,
    pub categories: Vec<Category>,
    pub min_confidence: Confidence,
    pub fail_on_unmet_criteria: bool,
    pub require_all_criteria_met: bool,
    pub fail_on_incomplete: bool,
    pub fail_on_test_failure: bool,
    pub require_must_pass_tests: bool,
    pub min_edge_pass_pct: u32,
    pub fail_on_rule_violation: bool,
    pub fail_on_design: Vec<DesignMismatchKind>,
    pub require_reproduce_for_critical: bool,
    pub unverifiable: UnverifiablePolicy,
}

impl Default for GatePolicy {
    fn default() -> Self {
        Self {
            fail_on: Severity::Critical,
            categories: vec![
                Category::Security,
                Category::Correctness,
                Category::Concurrency,
                Category::DataLoss,
                Category::Architecture,
            ],
            min_confidence: Confidence::High,
            fail_on_unmet_criteria: true,
            require_all_criteria_met: true,
            fail_on_incomplete: true,
            fail_on_test_failure: false,
            require_must_pass_tests: true,
            min_edge_pass_pct: 80,
            fail_on_rule_violation: false,
            fail_on_design: Vec::new(),
            require_reproduce_for_critical: true,
            unverifiable: UnverifiablePolicy::Strict,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GateDecision {
    pub passed: bool,
    pub blocking: Vec<usize>,
    pub unmet_criteria: Vec<String>,
    pub failed_checks: Vec<String>,
    #[serde(default)]
    pub incomplete: bool,
    #[serde(default)]
    pub failed_tests: Vec<String>,
    #[serde(default)]
    pub advisory_tests: Vec<String>,
    #[serde(default)]
    pub rule_violations: Vec<String>,
    #[serde(default)]
    pub design_mismatches: Vec<String>,
    #[serde(default)]
    pub unverified_criteria: Vec<String>,
}

pub struct GateInput<'a> {
    pub findings: &'a [Finding],
    pub requirements: Option<&'a RequirementsReport>,
    pub checks: &'a [CheckResult],
    pub incomplete: bool,
    pub agents: Option<&'a AgentReport>,
}

impl GatePolicy {
    pub fn is_blocking(&self, finding: &Finding) -> bool {
        finding.feedback != Some(FeedbackMark::Dismissed)
            && finding.severity >= self.fail_on
            && finding.confidence >= self.min_confidence
            && self.categories.contains(&finding.category)
    }

    pub fn decide(&self, input: GateInput<'_>) -> GateDecision {
        let blocking: Vec<usize> =
            input.findings.iter().enumerate().filter(|(_, f)| self.is_blocking(f)).map(|(i, _)| i).collect();
        let (unmet_criteria, unverified_criteria) = self.criteria_outcome(input.requirements);
        let failed_checks: Vec<String> =
            input.checks.iter().filter(|c| c.is_blocking()).map(|c| c.name.clone()).collect();
        let criteria_ok = !self.fail_on_unmet_criteria || unmet_criteria.is_empty();
        let completeness_ok = self.is_advisory() || !self.fail_on_incomplete || !input.incomplete;
        let (rule_violations, design_mismatches) =
            input.agents.map_or_else(Default::default, |a| self.agent_violations(a));
        let (tests_block, failed_tests, advisory_tests) =
            input.agents.map_or((false, Vec::new(), Vec::new()), |a| self.test_gate(a, input.requirements));
        let agents_ok = !tests_block
            && (!self.fail_on_rule_violation || rule_violations.is_empty())
            && design_mismatches.is_empty();
        GateDecision {
            passed: blocking.is_empty() && criteria_ok && failed_checks.is_empty() && completeness_ok && agents_ok,
            blocking,
            unmet_criteria,
            failed_checks,
            incomplete: input.incomplete,
            failed_tests,
            advisory_tests,
            rule_violations,
            design_mismatches,
            unverified_criteria,
        }
    }

    fn is_advisory(&self) -> bool {
        self.unverifiable == UnverifiablePolicy::Advisory
    }

    fn criteria_outcome(&self, requirements: Option<&RequirementsReport>) -> (Vec<String>, Vec<String>) {
        let Some(report) = requirements else {
            return (Vec::new(), Vec::new());
        };
        let owned = |ids: Vec<&str>| -> Vec<String> {
            ids.into_iter().filter(|id| !report.is_inferred(id)).map(str::to_string).collect()
        };
        let blocking =
            if self.require_all_criteria_met && !self.is_advisory() { report.unsatisfied() } else { report.unmet() };
        (owned(blocking), owned(report.unverified()))
    }

    fn test_gate(
        &self,
        agents: &AgentReport,
        requirements: Option<&RequirementsReport>,
    ) -> (bool, Vec<String>, Vec<String>) {
        let failed: Vec<String> = agents
            .tests
            .iter()
            .filter(|t| t.verdict == TestVerdict::FailedAssertion)
            .map(|t| t.scenario_id.clone())
            .collect();
        let Some(brief) = &agents.brief else {
            return (self.fail_on_test_failure && !failed.is_empty(), failed, Vec::new());
        };
        let inferred = |id: &String| requirements.is_some_and(|r| r.is_inferred(id));
        let scenario = |id: &str| brief.scenarios.iter().find(|s| s.id == id);
        let must_pass = |id: &str| scenario(id).is_some_and(|s| !s.criterion_ids.is_empty());
        let advisory_only = |id: &str| {
            scenario(id).is_some_and(|s| !s.criterion_ids.is_empty() && s.criterion_ids.iter().all(inferred))
        };
        let mut must_failed = false;
        let mut unverified = Vec::new();
        let mut inferred_unpassed = Vec::new();
        let mut edge_total = 0u32;
        let mut edge_pass = 0u32;
        let mut edge_unpassed = Vec::new();
        let mut edge_unpassed_advisory = Vec::new();
        for test in &agents.tests {
            if advisory_only(&test.scenario_id) {
                if test.verdict != TestVerdict::Passed {
                    inferred_unpassed.push(test.scenario_id.clone());
                }
            } else if must_pass(&test.scenario_id) {
                match test.verdict {
                    TestVerdict::Passed => {}
                    TestVerdict::FailedAssertion => must_failed = true,
                    _ => unverified.push(test.scenario_id.clone()),
                }
            } else if self.is_advisory() && test.verdict == TestVerdict::Inconclusive {
                edge_unpassed_advisory.push(test.scenario_id.clone());
            } else {
                edge_total += 1;
                if test.verdict == TestVerdict::Passed {
                    edge_pass += 1;
                } else {
                    edge_unpassed.push(test.scenario_id.clone());
                }
            }
        }
        let edge_ok = edge_total == 0 || edge_pass * 100 >= edge_total * self.min_edge_pass_pct;
        let must_unverified_blocks = !self.is_advisory() && self.fail_on_test_failure && !unverified.is_empty();
        let edge_blocks = !edge_ok || (self.fail_on_test_failure && !edge_unpassed.is_empty());
        let block = (self.require_must_pass_tests && must_failed) || edge_blocks || must_unverified_blocks;
        let mut advisory = unverified;
        advisory.extend(inferred_unpassed);
        if !edge_blocks {
            advisory.extend(edge_unpassed);
        }
        advisory.extend(edge_unpassed_advisory);
        (block, failed, advisory)
    }

    fn agent_violations(&self, agents: &AgentReport) -> (Vec<String>, Vec<String>) {
        let mut rule_violations: Vec<String> =
            agents.violations.iter().filter(|v| v.approved).map(|v| v.rule_id.clone()).collect();
        rule_violations.dedup();
        let design_mismatches = agents
            .design
            .iter()
            .filter(|d| d.kind != DesignMismatchKind::States && self.fail_on_design.contains(&d.kind))
            .map(|d| format!("{:?}:{}", d.kind, d.node_id).to_lowercase())
            .collect();
        (rule_violations, design_mismatches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::checks::CheckOutcome;
    use crate::review::domain::finding::fixtures::finding;
    use crate::review::domain::unverifiable::UnverifiableReason;

    fn only(findings: &[Finding]) -> GateInput<'_> {
        GateInput { findings, requirements: None, checks: &[], incomplete: false, agents: None }
    }

    #[test]
    fn critical_high_confidence_correctness_blocks() {
        let decision = GatePolicy::default().decide(only(&[finding("a.go", 1, Severity::Critical, "x")]));
        assert!(!decision.passed);
        assert_eq!(decision.blocking, vec![0]);
    }

    #[test]
    fn dismissed_finding_never_blocks_even_when_security() {
        let mut dismissed = finding("a.go", 1, Severity::Critical, "x");
        dismissed.category = Category::Security;
        dismissed.feedback = Some(FeedbackMark::Dismissed);
        let decision = GatePolicy::default().decide(only(&[dismissed]));
        assert!(decision.passed);
        assert!(decision.blocking.is_empty());
    }

    #[test]
    fn below_threshold_severity_or_confidence_does_not_block() {
        let major = finding("a.go", 1, Severity::Major, "x");
        let mut unsure = finding("a.go", 2, Severity::Critical, "x");
        unsure.confidence = Confidence::Medium;
        let decision = GatePolicy::default().decide(only(&[major, unsure]));
        assert!(decision.passed);
    }

    #[test]
    fn non_blocking_category_does_not_block() {
        let mut f = finding("a.go", 1, Severity::Critical, "x");
        f.category = Category::Maintainability;
        assert!(!GatePolicy::default().is_blocking(&f));
    }

    fn report_with_unmet() -> RequirementsReport {
        use crate::review::domain::requirements::*;
        RequirementsReport {
            requirements: Requirements { key: "K-1".into(), title: "t".into(), url: None, criteria: vec![] },
            assessments: vec![CriterionAssessment {
                criterion_id: "AC-1".into(),
                status: CriterionStatus::NotMet,
                explanation: String::new(),
                evidence: vec![],
                reason: None,
                retried: false,
                human: None,
            }],
        }
    }

    #[test]
    fn unmet_criterion_fails_gate_when_policy_requires_it() {
        let report = report_with_unmet();
        let decision = GatePolicy::default().decide(GateInput {
            findings: &[],
            requirements: Some(&report),
            checks: &[],
            incomplete: false,
            agents: None,
        });
        assert!(!decision.passed);
        assert_eq!(decision.unmet_criteria, vec!["AC-1"]);
        let lenient = GatePolicy { fail_on_unmet_criteria: false, ..GatePolicy::default() };
        assert!(
            lenient
                .decide(GateInput {
                    findings: &[],
                    requirements: Some(&report),
                    checks: &[],
                    incomplete: false,
                    agents: None
                })
                .passed
        );
    }

    #[test]
    fn unclear_criterion_blocks_only_when_all_criteria_must_be_met() {
        use crate::review::domain::requirements::*;
        let report = RequirementsReport {
            requirements: Requirements { key: "K-1".into(), title: "t".into(), url: None, criteria: vec![] },
            assessments: vec![
                CriterionAssessment {
                    criterion_id: "AC-1".into(),
                    status: CriterionStatus::Met,
                    explanation: String::new(),
                    evidence: vec![],
                    reason: None,
                    retried: false,
                    human: None,
                },
                CriterionAssessment {
                    criterion_id: "AC-2".into(),
                    status: CriterionStatus::Unclear,
                    explanation: String::new(),
                    evidence: vec![],
                    reason: None,
                    retried: false,
                    human: None,
                },
            ],
        };
        let input = |p: &GatePolicy| {
            p.decide(GateInput {
                findings: &[],
                requirements: Some(&report),
                checks: &[],
                incomplete: false,
                agents: None,
            })
        };
        let strict = input(&GatePolicy::default());
        assert!(!strict.passed, "an unclear AC is not demonstrably met, so it blocks");
        assert_eq!(strict.unmet_criteria, vec!["AC-2"]);
        let lenient = input(&GatePolicy { require_all_criteria_met: false, ..GatePolicy::default() });
        assert!(lenient.passed, "with the relaxed policy only not_met blocks");
        assert!(lenient.unmet_criteria.is_empty());
    }

    fn inferred_report(status: crate::review::domain::requirements::CriterionStatus) -> RequirementsReport {
        use crate::review::domain::requirements::*;
        RequirementsReport {
            requirements: Requirements {
                key: "K-1".into(),
                title: "t".into(),
                url: None,
                criteria: vec![AcceptanceCriterion::inferred("K-1/AC-I1", "wrong password -> Invalid credential")],
            },
            assessments: vec![CriterionAssessment {
                criterion_id: "K-1/AC-I1".into(),
                status,
                explanation: String::new(),
                evidence: vec![],
                reason: None,
                retried: false,
                human: None,
            }],
        }
    }

    #[test]
    fn an_unmet_inferred_criterion_never_blocks() {
        use crate::review::domain::requirements::CriterionStatus;
        for status in [CriterionStatus::NotMet, CriterionStatus::Unclear] {
            let report = inferred_report(status);

            let decision = GatePolicy::default().decide(GateInput {
                findings: &[],
                requirements: Some(&report),
                checks: &[],
                incomplete: false,
                agents: None,
            });

            assert!(decision.passed, "{status:?}");
            assert!(decision.unmet_criteria.is_empty());
        }
    }

    #[test]
    fn a_failing_test_bound_only_to_inferred_criteria_is_advisory() {
        use crate::review::domain::agent_results::*;
        use crate::review::domain::requirements::CriterionStatus;
        let report = inferred_report(CriterionStatus::Met);
        let scenario = |id: &str, acs: &[&str]| TestScenario {
            id: id.into(),
            criterion_ids: acs.iter().map(|a| (*a).to_string()).collect(),
            title: id.into(),
            given: String::new(),
            when: String::new(),
            then: "x".into(),
        };
        let result = |id: &str| TestResult {
            scenario_id: id.into(),
            language: "go".into(),
            verdict: TestVerdict::FailedAssertion,
            evidence: None,
            note: String::new(),
            reason: None,
        };
        let agents = AgentReport {
            brief: Some(TicketBrief { scenarios: vec![scenario("S1", &["K-1/AC-I1"])], ..TicketBrief::default() }),
            tests: vec![result("S1")],
            ..AgentReport::default()
        };

        let decision = GatePolicy::default().decide(GateInput {
            findings: &[],
            requirements: Some(&report),
            checks: &[],
            incomplete: false,
            agents: Some(&agents),
        });

        assert!(decision.passed);
        assert_eq!(decision.advisory_tests, vec!["S1"]);
        let mixed = AgentReport {
            brief: Some(TicketBrief {
                scenarios: vec![scenario("S1", &["K-1/AC-I1", "K-1/AC-1"])],
                ..TicketBrief::default()
            }),
            ..agents
        };
        let blocked = GatePolicy::default().decide(GateInput {
            findings: &[],
            requirements: Some(&report),
            checks: &[],
            incomplete: false,
            agents: Some(&mixed),
        });
        assert!(!blocked.passed, "a scenario that also covers a written criterion stays must-pass");
    }

    #[test]
    fn test_tiers_block_must_pass_and_enforce_edge_coverage() {
        use crate::review::domain::agent_results::*;

        let scenario = |id: &str, acs: &[&str]| TestScenario {
            id: id.into(),
            criterion_ids: acs.iter().map(|a| (*a).to_string()).collect(),
            title: id.into(),
            given: String::new(),
            when: String::new(),
            then: String::new(),
        };
        let result = |id: &str, verdict: TestVerdict| TestResult {
            scenario_id: id.into(),
            language: "go".into(),
            verdict,
            evidence: None,
            note: String::new(),
            reason: None,
        };
        let report = |scenarios: Vec<TestScenario>, tests: Vec<TestResult>| AgentReport {
            brief: Some(TicketBrief { scenarios, ..TicketBrief::default() }),
            tests,
            ..AgentReport::default()
        };
        let decide = |agents: &AgentReport| {
            GatePolicy::default().decide(GateInput {
                findings: &[],
                requirements: None,
                checks: &[],
                incomplete: false,
                agents: Some(agents),
            })
        };

        let must_fail = report(vec![scenario("M1", &["AC-1"])], vec![result("M1", TestVerdict::FailedAssertion)]);
        assert!(!decide(&must_fail).passed, "a must-pass (AC-bound) scenario that fails blocks");

        let edges = vec![
            scenario("E1", &[]),
            scenario("E2", &[]),
            scenario("E3", &[]),
            scenario("E4", &[]),
            scenario("E5", &[]),
        ];
        let at_threshold = report(
            edges.clone(),
            vec![
                result("E1", TestVerdict::Passed),
                result("E2", TestVerdict::Passed),
                result("E3", TestVerdict::Passed),
                result("E4", TestVerdict::Passed),
                result("E5", TestVerdict::FailedAssertion),
            ],
        );
        let decision = decide(&at_threshold);
        assert!(decision.passed, "edge coverage at 80% passes");
        assert_eq!(decision.advisory_tests, vec!["E5"], "the un-passed edge case is a red-warning advisory");

        let below_threshold = report(
            edges,
            vec![
                result("E1", TestVerdict::Passed),
                result("E2", TestVerdict::Passed),
                result("E3", TestVerdict::Passed),
                result("E4", TestVerdict::FailedAssertion),
                result("E5", TestVerdict::FailedAssertion),
            ],
        );
        let decision = decide(&below_threshold);
        assert!(!decision.passed, "edge coverage below 80% blocks");
        assert!(decision.advisory_tests.is_empty());

        let unrunnable = report(
            vec![scenario("M1", &["AC-1"]), scenario("M2", &["AC-2"])],
            vec![result("M1", TestVerdict::Inconclusive), result("M2", TestVerdict::Inconclusive)],
        );
        let lenient = decide(&unrunnable);
        assert!(lenient.passed, "must-pass tests that could not run are advisory, not blocking, by default");
        assert_eq!(lenient.advisory_tests, vec!["M1", "M2"]);
        assert!(lenient.failed_tests.is_empty());
        let strict = GatePolicy { fail_on_test_failure: true, ..GatePolicy::default() }.decide(GateInput {
            findings: &[],
            requirements: None,
            checks: &[],
            incomplete: false,
            agents: Some(&unrunnable),
        });
        assert!(!strict.passed, "strict policy blocks when must-pass tests cannot be verified");
    }

    #[test]
    fn failed_required_check_fails_gate() {
        let checks = [CheckResult {
            name: "unit-tests".into(),
            required: true,
            outcome: CheckOutcome::Failed,
            exit_code: Some(1),
            duration_ms: 10,
            output_tail: String::new(),
        }];
        let decision = GatePolicy::default().decide(GateInput {
            findings: &[],
            requirements: None,
            checks: &checks,
            incomplete: false,
            agents: None,
        });
        assert!(!decision.passed);
        assert_eq!(decision.failed_checks, vec!["unit-tests"]);
    }

    #[test]
    fn stricter_policy_blocks_major() {
        let policy = GatePolicy { fail_on: Severity::Major, ..GatePolicy::default() };
        assert!(policy.is_blocking(&finding("a.go", 1, Severity::Major, "x")));
    }

    #[test]
    fn incomplete_review_fails_gate_unless_policy_allows_it() {
        let input = || GateInput { findings: &[], requirements: None, checks: &[], incomplete: true, agents: None };
        let decision = GatePolicy::default().decide(input());
        assert!(!decision.passed);
        assert!(decision.incomplete);
        let lenient = GatePolicy { fail_on_incomplete: false, ..GatePolicy::default() };
        assert!(lenient.decide(input()).passed);
    }

    #[test]
    fn agent_results_block_only_when_policy_says_so() {
        use crate::review::domain::agent_results::*;
        let agents = AgentReport {
            tests: vec![TestResult {
                scenario_id: "S1".into(),
                language: "go".into(),
                verdict: TestVerdict::FailedAssertion,
                evidence: None,
                note: String::new(),
                reason: None,
            }],
            violations: vec![RuleViolation {
                rule_id: "R1".into(),
                rule_title: "layering".into(),
                approved: true,
                file: "a.go".into(),
                line: 1,
                evidence: "x".into(),
                explanation: String::new(),
                suggestion: None,
            }],
            design: vec![DesignMismatch {
                kind: DesignMismatchKind::States,
                frame: "f".into(),
                node_id: "1:2".into(),
                design_value: "loading".into(),
                file: "a.go".into(),
                line: 1,
                code_evidence: "x".into(),
                explanation: String::new(),
            }],
            ..AgentReport::default()
        };
        let input =
            || GateInput { findings: &[], requirements: None, checks: &[], incomplete: false, agents: Some(&agents) };
        let lenient = GatePolicy::default().decide(input());
        assert!(lenient.passed, "warn-only by default");
        assert_eq!(lenient.failed_tests, vec!["S1"]);
        assert_eq!(lenient.rule_violations, vec!["R1"]);
        let strict = GatePolicy {
            fail_on_test_failure: true,
            fail_on_rule_violation: true,
            fail_on_design: vec![DesignMismatchKind::States],
            ..GatePolicy::default()
        };
        let decision = strict.decide(input());
        assert!(!decision.passed);
        assert!(decision.design_mismatches.is_empty(), "states never blocks");
    }

    fn unclear_report() -> RequirementsReport {
        use crate::review::domain::requirements::*;
        let unclear = |id: &str| CriterionAssessment::unclear(id, UnverifiableReason::NeedsContext, String::new());
        let mut not_applicable = unclear("AC-3");
        not_applicable.human = Some(HumanMark::NotApplicable);
        RequirementsReport {
            requirements: Requirements {
                key: "K-1".into(),
                title: "t".into(),
                url: None,
                criteria: vec![AcceptanceCriterion::inferred("AC-4", "x")],
            },
            assessments: vec![unclear("AC-1"), unclear("AC-4"), not_applicable],
        }
    }

    fn decide_with(policy: &GatePolicy, report: &RequirementsReport, incomplete: bool) -> GateDecision {
        policy.decide(GateInput { findings: &[], requirements: Some(report), checks: &[], incomplete, agents: None })
    }

    #[test]
    fn strict_policy_blocks_on_unclear_and_still_lists_them_as_unverified() {
        let decision = decide_with(&GatePolicy::default(), &unclear_report(), false);
        assert!(!decision.passed);
        assert_eq!(decision.unmet_criteria, vec!["AC-1"]);
        assert_eq!(decision.unverified_criteria, vec!["AC-1"]);
    }

    #[test]
    fn advisory_policy_never_blocks_on_unverifiable_work() {
        let advisory = GatePolicy { unverifiable: UnverifiablePolicy::Advisory, ..GatePolicy::default() };
        let decision = decide_with(&advisory, &unclear_report(), true);
        assert!(decision.passed);
        assert!(decision.unmet_criteria.is_empty());
        assert_eq!(decision.unverified_criteria, vec!["AC-1"]);
        assert!(decision.incomplete, "still reported as incomplete");
    }

    #[test]
    fn advisory_policy_still_blocks_on_not_met() {
        let advisory = GatePolicy { unverifiable: UnverifiablePolicy::Advisory, ..GatePolicy::default() };
        let decision = decide_with(&advisory, &report_with_unmet(), false);
        assert!(!decision.passed);
        assert_eq!(decision.unmet_criteria, vec!["AC-1"]);
        assert!(decision.unverified_criteria.is_empty());
    }

    #[test]
    fn advisory_policy_keeps_inconclusive_tests_advisory_even_when_tests_must_pass() {
        use crate::review::domain::agent_results::*;
        let scenario = |id: &str, acs: &[&str]| TestScenario {
            id: id.into(),
            criterion_ids: acs.iter().map(|a| (*a).to_string()).collect(),
            title: id.into(),
            given: String::new(),
            when: String::new(),
            then: String::new(),
        };
        let result = |id: &str, verdict| TestResult {
            scenario_id: id.into(),
            language: "go".into(),
            verdict,
            evidence: None,
            note: String::new(),
            reason: None,
        };
        let agents = AgentReport {
            brief: Some(TicketBrief {
                scenarios: vec![scenario("M1", &["AC-1"]), scenario("E1", &[])],
                ..TicketBrief::default()
            }),
            tests: vec![result("M1", TestVerdict::Inconclusive), result("E1", TestVerdict::Inconclusive)],
            ..AgentReport::default()
        };
        let input =
            || GateInput { findings: &[], requirements: None, checks: &[], incomplete: false, agents: Some(&agents) };
        let strict = GatePolicy { fail_on_test_failure: true, ..GatePolicy::default() };
        assert!(!strict.decide(input()).passed);
        let advisory = GatePolicy { unverifiable: UnverifiablePolicy::Advisory, ..strict };
        let decision = advisory.decide(input());
        assert!(decision.passed);
        assert_eq!(decision.advisory_tests, vec!["M1", "E1"]);
    }

    #[test]
    fn unverifiable_policy_parses_its_names() {
        for policy in UnverifiablePolicy::ALL {
            assert_eq!(UnverifiablePolicy::parse(policy.as_str()), Some(policy));
            assert_eq!(serde_json::to_value(policy).unwrap(), policy.as_str());
        }
        assert_eq!(UnverifiablePolicy::parse(" Advisory "), Some(UnverifiablePolicy::Advisory));
        assert_eq!(UnverifiablePolicy::parse("lax"), None);
        assert_eq!(UnverifiablePolicy::default(), UnverifiablePolicy::Strict);
    }
}
