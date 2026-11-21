use crate::review::application::ReviewReport;
use crate::review::domain::agent_results::{AgentReport, TestScenario};

pub const REDACTED: &str = "[REDACTED]";
const TOKEN_PREFIXES: &[&str] = &["sk-ant-", "ghp_", "github_pat_", "xoxb-", "xoxp-", "AKIA", "ATATT"];
const MIN_TOKEN_BODY: usize = 12;
const MIN_SECRET_LEN: usize = 8;

pub fn redact_report(report: &mut ReviewReport, secrets: &[&str]) {
    let clean = |text: &mut String| *text = redact_text(text, secrets);
    for check in &mut report.checks {
        clean(&mut check.output_tail);
    }
    for finding in report.findings.iter_mut().chain(report.rejected.iter_mut().map(|r| &mut r.finding)) {
        clean(&mut finding.evidence);
        clean(&mut finding.explanation);
        if let Some(suggestion) = finding.suggestion.as_mut() {
            clean(suggestion);
        }
        if let Some(rule) = finding.prevention_rule.as_mut() {
            clean(&mut rule.text);
            rule.scope.iter_mut().for_each(clean);
        }
    }
    if let Some(requirements) = report.requirements.as_mut() {
        for evidence in requirements.assessments.iter_mut().flat_map(|a| a.evidence.iter_mut()) {
            clean(&mut evidence.quote);
        }
    }
    if let Some(agents) = report.agents.as_mut() {
        redact_agents(agents, &clean);
    }
}

fn redact_agents(agents: &mut AgentReport, clean: &impl Fn(&mut String)) {
    if let Some(brief) = agents.brief.as_mut() {
        clean(&mut brief.context);
        brief.implicit_requirements.iter_mut().for_each(clean);
        brief.scenarios.iter_mut().for_each(|s| clean_scenario(s, clean));
    }
    for test in &mut agents.tests {
        clean(&mut test.note);
        if let Some(evidence) = test.evidence.as_mut() {
            clean(&mut evidence.test_source);
            clean(&mut evidence.output);
            clean(&mut evidence.reproduce);
            evidence.command.iter_mut().for_each(clean);
            evidence.criterion_texts.iter_mut().for_each(clean);
            evidence.code_under_test.iter_mut().for_each(|e| clean(&mut e.quote));
            clean_scenario(&mut evidence.scenario, clean);
        }
    }
    for violation in &mut agents.violations {
        clean(&mut violation.evidence);
        clean(&mut violation.explanation);
        violation.suggestion.iter_mut().for_each(clean);
    }
    for mismatch in &mut agents.design {
        clean(&mut mismatch.design_value);
        clean(&mut mismatch.code_evidence);
        clean(&mut mismatch.explanation);
    }
    agents.notes.iter_mut().for_each(clean);
}

fn clean_scenario(scenario: &mut TestScenario, clean: &impl Fn(&mut String)) {
    for text in [&mut scenario.title, &mut scenario.given, &mut scenario.when, &mut scenario.then] {
        clean(text);
    }
}

pub fn redact_text(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets.iter().map(|s| s.trim()).filter(|s| s.len() >= MIN_SECRET_LEN) {
        out = out.replace(secret, REDACTED);
    }
    for prefix in TOKEN_PREFIXES {
        out = redact_prefixed_tokens(&out, prefix);
    }
    out
}

fn redact_prefixed_tokens(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(prefix) {
        let after = &rest[start + prefix.len()..];
        let body_len =
            after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')).unwrap_or(after.len());
        out.push_str(&rest[..start]);
        if body_len >= MIN_TOKEN_BODY {
            out.push_str(REDACTED);
        } else {
            out.push_str(&rest[start..start + prefix.len() + body_len]);
        }
        rest = &after[body_len..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_secret_values_and_token_patterns() {
        let text = "env: JIRA=jira-token-123456 KEY=sk-ant-api03-abcdefghijklmnop other=ok";
        let out = redact_text(text, &["jira-token-123456"]);
        assert_eq!(out, "env: JIRA=[REDACTED] KEY=[REDACTED] other=ok");
    }

    #[test]
    fn leaves_short_values_and_short_prefixed_words_alone() {
        assert_eq!(redact_text("key=abc AKIA ok", &["abc"]), "key=abc AKIA ok");
        assert_eq!(redact_text("sk-ant-short", &[]), "sk-ant-short");
    }

    #[test]
    fn redacts_every_text_field_of_a_report() {
        use crate::review::application::{CheckOutcome, CheckResult, TokenUsage};
        use crate::review::domain::gate::GateDecision;
        let mut report = ReviewReport {
            files_reviewed: vec![],
            skipped: vec![],
            findings: vec![],
            rejected: vec![],
            requirements: None,
            checks: vec![CheckResult {
                name: "env".into(),
                required: true,
                outcome: CheckOutcome::Passed,
                exit_code: Some(0),
                duration_ms: 1,
                output_tail: "ANTHROPIC_API_KEY=sk-ant-api03-zzzzzzzzzzzzzzzz".into(),
            }],
            agents: None,
            ticket: None,
            gate: GateDecision {
                passed: true,
                blocking: vec![],
                unmet_criteria: vec![],
                failed_checks: vec![],
                incomplete: false,
                ..GateDecision::default()
            },
            usage: TokenUsage::default(),
            estimated_cost_usd: None,
        };
        report.findings.push(crate::review::domain::finding::fixtures::finding(
            "a.go",
            1,
            crate::review::application::Severity::Major,
            "token := \"ghp_abcdefghijklmnopqrst\"",
        ));
        report.findings[0].prevention_rule =
            crate::review::application::PreventionRule::new("Never hardcode tokens like ghp_abcdefghijklmnopqrst", "");
        report.rejected.push(crate::review::domain::verification::RejectedFinding {
            finding: report.findings[0].clone(),
            reason: crate::review::domain::verification::RejectReason::EvidenceNotFound,
        });
        redact_report(&mut report, &[]);
        assert_eq!(report.checks[0].output_tail, "ANTHROPIC_API_KEY=[REDACTED]");
        assert_eq!(report.findings[0].prevention_rule.as_ref().unwrap().text, "Never hardcode tokens like [REDACTED]");
        assert_eq!(
            report.rejected[0].finding.prevention_rule.as_ref().unwrap().text,
            "Never hardcode tokens like [REDACTED]"
        );
    }

    #[test]
    fn redacts_every_agent_field() {
        use crate::review::domain::agent_results::*;
        use crate::review::domain::requirements::EvidenceRef;
        const S: &str = "figma-secret-value-123";
        let scenario = TestScenario {
            id: "S1".into(),
            criterion_ids: vec![],
            title: S.into(),
            given: S.into(),
            when: S.into(),
            then: S.into(),
        };
        let mut agents = AgentReport {
            brief: Some(TicketBrief {
                scenarios: vec![scenario.clone()],
                implicit_requirements: vec![S.into()],
                context: S.into(),
            }),
            tests: vec![TestResult {
                scenario_id: "S1".into(),
                language: "go".into(),
                verdict: TestVerdict::FailedAssertion,
                evidence: Some(TestEvidence {
                    criterion_ids: vec![],
                    criterion_texts: vec![S.into()],
                    scenario,
                    test_file: "a_test.go".into(),
                    test_source: S.into(),
                    command: vec![S.into()],
                    exit_code: Some(1),
                    output: S.into(),
                    code_under_test: vec![EvidenceRef { file: "a.go".into(), line: 1, quote: S.into() }],
                    commit: "abc".into(),
                    reproduce: S.into(),
                }),
                note: S.into(),
            }],
            violations: vec![RuleViolation {
                rule_id: "R1".into(),
                rule_title: "t".into(),
                approved: true,
                file: "a.go".into(),
                line: 1,
                evidence: S.into(),
                explanation: S.into(),
                suggestion: Some(S.into()),
            }],
            design: vec![DesignMismatch {
                kind: DesignMismatchKind::Copy,
                frame: "f".into(),
                node_id: "1:2".into(),
                design_value: S.into(),
                file: "a.go".into(),
                line: 1,
                code_evidence: S.into(),
                explanation: S.into(),
            }],
            notes: vec![S.into()],
            cost_usd: None,
        };
        let clean = |t: &mut String| *t = redact_text(t, &[S]);
        redact_agents(&mut agents, &clean);
        let json = serde_json::to_string(&agents).unwrap();
        assert!(!json.contains(S), "secret survived: {json}");
    }
}
