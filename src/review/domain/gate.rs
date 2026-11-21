use serde::Serialize;

use super::agent_results::{AgentReport, DesignMismatchKind, TestVerdict};
use super::checks::CheckResult;
use super::finding::{Category, Confidence, Finding, Severity};
use super::requirements::RequirementsReport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatePolicy {
    pub fail_on: Severity,
    pub categories: Vec<Category>,
    pub min_confidence: Confidence,
    pub fail_on_unmet_criteria: bool,
    pub fail_on_incomplete: bool,
    pub fail_on_test_failure: bool,
    pub fail_on_rule_violation: bool,
    pub fail_on_design: Vec<DesignMismatchKind>,
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
            fail_on_incomplete: true,
            fail_on_test_failure: false,
            fail_on_rule_violation: false,
            fail_on_design: Vec::new(),
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
    pub rule_violations: Vec<String>,
    #[serde(default)]
    pub design_mismatches: Vec<String>,
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
        finding.severity >= self.fail_on
            && finding.confidence >= self.min_confidence
            && self.categories.contains(&finding.category)
    }

    pub fn decide(&self, input: GateInput<'_>) -> GateDecision {
        let blocking: Vec<usize> =
            input.findings.iter().enumerate().filter(|(_, f)| self.is_blocking(f)).map(|(i, _)| i).collect();
        let unmet_criteria: Vec<String> =
            input.requirements.map(|r| r.unmet().into_iter().map(str::to_string).collect()).unwrap_or_default();
        let failed_checks: Vec<String> =
            input.checks.iter().filter(|c| c.is_blocking()).map(|c| c.name.clone()).collect();
        let criteria_ok = !self.fail_on_unmet_criteria || unmet_criteria.is_empty();
        let completeness_ok = !self.fail_on_incomplete || !input.incomplete;
        let (failed_tests, rule_violations, design_mismatches) =
            input.agents.map_or_else(Default::default, |a| self.agent_failures(a));
        let agents_ok = (!self.fail_on_test_failure || failed_tests.is_empty())
            && (!self.fail_on_rule_violation || rule_violations.is_empty())
            && design_mismatches.is_empty();
        GateDecision {
            passed: blocking.is_empty() && criteria_ok && failed_checks.is_empty() && completeness_ok && agents_ok,
            blocking,
            unmet_criteria,
            failed_checks,
            incomplete: input.incomplete,
            failed_tests,
            rule_violations,
            design_mismatches,
        }
    }

    fn agent_failures(&self, agents: &AgentReport) -> (Vec<String>, Vec<String>, Vec<String>) {
        let failed_tests = agents
            .tests
            .iter()
            .filter(|t| t.verdict == TestVerdict::FailedAssertion)
            .map(|t| t.scenario_id.clone())
            .collect();
        let mut rule_violations: Vec<String> =
            agents.violations.iter().filter(|v| v.approved).map(|v| v.rule_id.clone()).collect();
        rule_violations.dedup();
        let design_mismatches = agents
            .design
            .iter()
            .filter(|d| d.kind != DesignMismatchKind::States && self.fail_on_design.contains(&d.kind))
            .map(|d| format!("{:?}:{}", d.kind, d.node_id).to_lowercase())
            .collect();
        (failed_tests, rule_violations, design_mismatches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::checks::CheckOutcome;
    use crate::review::domain::finding::fixtures::finding;

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
}
