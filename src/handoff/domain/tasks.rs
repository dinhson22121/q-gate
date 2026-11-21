use super::document::{CodeBlock, Fact, HandoffInput, Task, TaskKind};
use super::text::language_of;
use crate::review::application::{
    CheckOutcome, CheckResult, CriterionStatus, DesignMismatch, DesignMismatchKind, EvidenceRef, Finding,
    RequirementsReport, RuleViolation, Severity, TestEvidence, TestResult, TestVerdict,
};

const DO_NOT_EDIT_TEST: &str =
    "Change the production code so this test passes. Do not weaken, skip or delete the generated test.";

pub(super) fn collect(input: &HandoffInput<'_>) -> (Vec<Task>, Vec<Task>) {
    let report = input.report;
    let mut rules = input.rules;
    if report.gate.passed {
        rules.failed_tests = false;
        rules.rule_violations = false;
        rules.unmet_criteria = false;
    }
    let (mut blocking, mut warnings) = (Vec::new(), Vec::new());

    let mut checks: Vec<&CheckResult> = report.checks.iter().filter(|c| c.outcome != CheckOutcome::Passed).collect();
    checks.sort_by(|a, b| a.name.cmp(&b.name));
    let (required, optional): (Vec<_>, Vec<_>) = checks.into_iter().partition(|c| c.is_blocking());
    blocking.extend(required.into_iter().map(|c| check_task(c, TaskKind::FailedCheck)));

    let mut findings: Vec<(bool, &Finding)> =
        report.findings.iter().enumerate().map(|(i, f)| (report.gate.blocking.contains(&i), f)).collect();
    findings.sort_by(|(_, a), (_, b)| b.severity.cmp(&a.severity).then(a.file.cmp(&b.file)).then(a.line.cmp(&b.line)));
    for (is_blocking, finding) in findings {
        if is_blocking {
            blocking.push(finding_task(finding));
        } else if finding.severity >= Severity::Minor {
            warnings.push(finding_task(finding));
        }
    }

    let agents = input.report.agents.as_ref();
    let mut tests: Vec<&TestResult> = agents.map(|a| a.tests.iter().collect()).unwrap_or_default();
    tests.sort_by(|a, b| a.scenario_id.cmp(&b.scenario_id));
    for test in tests.iter().filter(|t| t.verdict == TestVerdict::FailedAssertion) {
        let blocks = rules.failed_tests && report.gate.failed_tests.contains(&test.scenario_id);
        let target = if blocks { &mut blocking } else { &mut warnings };
        target.push(test_task(test, TaskKind::FailedTest));
    }

    let mut criteria_warnings = Vec::new();
    if let Some(requirements) = &report.requirements {
        for assessment in &requirements.assessments {
            match assessment.status {
                CriterionStatus::NotMet => {
                    let task = criterion_task(requirements, &assessment.criterion_id, TaskKind::UnmetCriterion);
                    if rules.unmet_criteria { blocking.push(task) } else { criteria_warnings.push(task) }
                }
                CriterionStatus::Unclear => criteria_warnings.push(criterion_task(
                    requirements,
                    &assessment.criterion_id,
                    TaskKind::UnclearCriterion,
                )),
                CriterionStatus::Met => {}
            }
        }
    }

    let mut violations: Vec<&RuleViolation> = agents.map(|a| a.violations.iter().collect()).unwrap_or_default();
    violations.sort_by(|a, b| a.rule_id.cmp(&b.rule_id).then(a.file.cmp(&b.file)).then(a.line.cmp(&b.line)));
    let mut design: Vec<&DesignMismatch> = agents.map(|a| a.design.iter().collect()).unwrap_or_default();
    design.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)).then(a.node_id.cmp(&b.node_id)));
    let blocks_design = |d: &DesignMismatch| report.gate.design_mismatches.contains(&design_key(d));

    for violation in &violations {
        if violation.approved && rules.rule_violations {
            blocking.push(violation_task(violation));
        }
    }
    blocking.extend(design.iter().filter(|d| blocks_design(d)).map(|d| design_task(d)));

    warnings.extend(
        tests
            .iter()
            .filter(|t| t.verdict == TestVerdict::Inconclusive)
            .map(|t| test_task(t, TaskKind::InconclusiveTest)),
    );
    warnings.extend(criteria_warnings);
    warnings.extend(violations.iter().filter(|v| !(v.approved && rules.rule_violations)).map(|v| violation_task(v)));
    warnings.extend(design.iter().filter(|d| !blocks_design(d)).map(|d| design_task(d)));
    warnings.extend(optional.into_iter().map(|c| check_task(c, TaskKind::OptionalCheck)));
    (blocking, warnings)
}

fn design_key(d: &DesignMismatch) -> String {
    format!("{:?}:{}", d.kind, d.node_id).to_lowercase()
}

fn fact(label: &str, value: impl Into<String>) -> Fact {
    Fact { label: label.into(), value: value.into() }
}

fn code(label: impl Into<String>, language: &str, content: &str) -> CodeBlock {
    CodeBlock { label: label.into(), language: language.into(), content: content.into() }
}

fn location(file: &str, line: u32) -> Option<String> {
    (!file.trim().is_empty()).then(|| format!("{file}:{line}"))
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn outcome_name(outcome: CheckOutcome) -> &'static str {
    match outcome {
        CheckOutcome::Passed => "passed",
        CheckOutcome::Failed => "failed",
        CheckOutcome::TimedOut => "timed out",
        CheckOutcome::Error => "could not start",
    }
}

fn check_task(check: &CheckResult, kind: TaskKind) -> Task {
    let required = if check.required { "Required" } else { "Optional" };
    let exit = check.exit_code.map_or_else(|| "none".to_string(), |c| c.to_string());
    let mut code_blocks = Vec::new();
    if !check.output_tail.trim().is_empty() {
        code_blocks.push(code("Output (last lines)", "text", &check.output_tail));
    }
    Task {
        kind,
        title: format!("{required} check \"{}\" {}", check.name, outcome_name(check.outcome)),
        location: None,
        problem: format!(
            "The check \"{}\" declared in the repository's .qgate.toml ([[checks]]) {} (exit code {exit}).",
            check.name,
            outcome_name(check.outcome)
        ),
        facts: vec![
            fact("Command", format!("the `command` of [[checks]] name = \"{}\" in .qgate.toml", check.name)),
            fact("Exit code", exit),
            fact("Duration", format!("{} ms", check.duration_ms)),
        ],
        code: code_blocks,
        expected: Some("The check passes (exit code 0).".into()),
        fix: Some("Run the check locally, fix the cause shown in the output, and run it again.".into()),
    }
}

fn finding_task(finding: &Finding) -> Task {
    let source = serde_json::to_value(&finding.source)
        .ok()
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .unwrap_or_else(|| "Claude review".to_string());
    Task {
        kind: TaskKind::Finding,
        title: finding.title.clone(),
        location: location(&finding.file, finding.line),
        problem: finding.explanation.clone(),
        facts: vec![
            fact("Severity", finding.severity.as_str()),
            fact("Category", finding.category.as_str()),
            fact("Confidence", finding.confidence.as_str()),
            fact("Source", source),
        ]
        .into_iter()
        .chain(finding.prevention_rule.as_ref().map(|rule| match &rule.scope {
            Some(scope) => fact("Rule", format!("{} (files: {scope})", rule.text)),
            None => fact("Rule", rule.text.as_str()),
        }))
        .collect(),
        code: vec![code("Evidence", language_of(&finding.file), &finding.evidence)],
        expected: None,
        fix: finding.suggestion.as_deref().and_then(non_empty),
    }
}

fn test_task(test: &TestResult, kind: TaskKind) -> Task {
    let failed = kind == TaskKind::FailedTest;
    let title = test.evidence.as_ref().map_or(String::new(), |e| format!(": {}", e.scenario.title.trim()));
    let mut task = Task {
        kind,
        title: if failed {
            format!("Generated test {} fails{title}", test.scenario_id)
        } else {
            format!("Generated test {} was inconclusive{title}", test.scenario_id)
        },
        location: None,
        problem: if failed {
            "A test generated from the acceptance criteria fails against this commit.".into()
        } else {
            format!("The test could not decide: {}", non_empty(&test.note).unwrap_or_else(|| "no reason given".into()))
        },
        facts: vec![fact("Language", test.language.as_str())],
        code: Vec::new(),
        expected: None,
        fix: Some(if failed {
            DO_NOT_EDIT_TEST.into()
        } else {
            "Check that the scenario is implemented; make it testable (or add your own test) if needed.".into()
        }),
    };
    if failed && let Some(note) = non_empty(&test.note) {
        task.problem = format!("{} {note}", task.problem);
    }
    if let Some(evidence) = &test.evidence {
        add_test_evidence(&mut task, evidence);
    }
    task
}

fn add_test_evidence(task: &mut Task, e: &TestEvidence) {
    task.location = e.code_under_test.first().and_then(|r| location(&r.file, r.line));
    for (index, id) in e.criterion_ids.iter().enumerate() {
        let text = e.criterion_texts.get(index).map(|t| t.trim()).unwrap_or_default();
        task.facts.push(fact("Criterion", if text.is_empty() { id.clone() } else { format!("{id}: {text}") }));
    }
    task.facts.push(fact("Given", e.scenario.given.trim()));
    task.facts.push(fact("When", e.scenario.when.trim()));
    task.facts.push(fact("Then", e.scenario.then.trim()));
    if !e.test_file.trim().is_empty() {
        task.facts.push(fact("Test file", e.test_file.trim()));
    }
    if let Some(code) = e.exit_code {
        task.facts.push(fact("Exit code", code.to_string()));
    }
    if !e.commit.trim().is_empty() {
        task.facts.push(fact("Commit", e.commit.trim()));
    }
    if !e.test_source.trim().is_empty() {
        task.code.push(code(
            format!("Test source ({})", e.test_file.trim()),
            language_of(&e.test_file),
            &e.test_source,
        ));
    }
    if !e.reproduce.trim().is_empty() {
        task.code.push(code("Reproduce", "sh", &e.reproduce));
    } else if !e.command.is_empty() {
        task.code.push(code("Command that was run", "sh", &e.command.join(" ")));
    }
    if !e.output.trim().is_empty() {
        task.code.push(code("Failing output", "text", &e.output));
    }
    task.code.extend(evidence_blocks("Code under test", &e.code_under_test));
    task.expected = non_empty(&e.scenario.then);
}

fn evidence_blocks(label: &str, refs: &[EvidenceRef]) -> Vec<CodeBlock> {
    refs.iter().map(|r| code(format!("{label} ({}:{})", r.file, r.line), language_of(&r.file), &r.quote)).collect()
}

pub(super) fn criterion_text(requirements: &RequirementsReport, id: &str) -> String {
    requirements.requirements.criteria.iter().find(|c| c.id == id).map(|c| c.text.clone()).unwrap_or_default()
}

fn criterion_task(requirements: &RequirementsReport, id: &str, kind: TaskKind) -> Task {
    let text = criterion_text(requirements, id);
    let assessment = requirements.assessments.iter().find(|a| a.criterion_id == id);
    let unmet = kind == TaskKind::UnmetCriterion;
    Task {
        kind,
        title: if unmet {
            format!("Acceptance criterion {id} is not met: {text}")
        } else {
            format!("Acceptance criterion {id} could not be confirmed: {text}")
        },
        location: assessment.and_then(|a| a.evidence.first()).and_then(|e| location(&e.file, e.line)),
        problem: assessment.map(|a| a.explanation.clone()).unwrap_or_default(),
        facts: vec![fact("Ticket", requirements.requirements.key.as_str())],
        code: assessment.map(|a| evidence_blocks("Related code", &a.evidence)).unwrap_or_default(),
        expected: non_empty(&text),
        fix: Some(if unmet {
            "Implement the behaviour the criterion describes, with a test that proves it.".into()
        } else {
            "Make the implementation of this criterion explicit (code and a test) so it can be verified.".into()
        }),
    }
}

fn violation_task(violation: &RuleViolation) -> Task {
    let status = if violation.approved { "approved" } else { "inferred (suggested, not approved)" };
    Task {
        kind: TaskKind::RuleViolation,
        title: format!("Architecture rule {} violated: {}", violation.rule_id, violation.rule_title),
        location: location(&violation.file, violation.line),
        problem: violation.explanation.clone(),
        facts: vec![fact("Rule", status)],
        code: vec![code("Evidence", language_of(&violation.file), &violation.evidence)],
        expected: Some(format!("The code follows rule {}: {}", violation.rule_id, violation.rule_title)),
        fix: violation.suggestion.as_deref().and_then(non_empty),
    }
}

fn design_task(mismatch: &DesignMismatch) -> Task {
    let kind = match mismatch.kind {
        DesignMismatchKind::Copy => "copy",
        DesignMismatchKind::Component => "component",
        DesignMismatchKind::Token => "token",
        DesignMismatchKind::States => "states",
    };
    Task {
        kind: TaskKind::DesignMismatch,
        title: format!("Design mismatch ({kind}) in Figma frame \"{}\"", mismatch.frame),
        location: location(&mismatch.file, mismatch.line),
        problem: mismatch.explanation.clone(),
        facts: vec![
            fact("Figma frame", format!("{} (node {})", mismatch.frame, mismatch.node_id)),
            fact("Design value", mismatch.design_value.as_str()),
        ],
        code: vec![code("Code", language_of(&mismatch.file), &mismatch.code_evidence)],
        expected: Some(format!("The code matches the design: {}", mismatch.design_value)),
        fix: None,
    }
}
