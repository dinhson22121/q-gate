use serde_json::Value;

use super::fixtures::*;
use super::*;
use crate::review::application::ReviewReport;

fn input(report: &ReviewReport) -> HandoffInput<'_> {
    HandoffInput { meta: meta(), report, rules: BlockingRules::default() }
}

fn headings(markdown: &str) -> Vec<&str> {
    markdown.lines().filter(|l| l.starts_with('#')).collect()
}

#[test]
fn failing_run_orders_blocking_then_warnings() {
    let report = failing_report();
    let markdown = render_markdown(&HandoffDocument::build(&input(&report)));
    assert_eq!(
        headings(&markdown),
        vec![
            "# qgate review: shop-api feature/SHOP-7-pay 3f9c2ab - FAILED",
            "## Do this first (blocking)",
            "### 1. Required check \"unit\" failed",
            "### 2. SQL injection",
            "### 3. Acceptance criterion AC-2 is not met: Receipt | is emailed",
            "### 4. Design mismatch (copy) in Figma frame \"Checkout / Pay\"",
            "## Should fix (warnings)",
            "### W1. Magic number",
            "### W2. Generated test S2 fails: Total includes tax",
            "### W3. Generated test S1 was inconclusive",
            "### W4. Acceptance criterion AC-3 could not be confirmed: Works offline",
            "### W5. Architecture rule R1 violated: R1 title",
            "### W6. Architecture rule R2 violated: R2 title",
            "### W7. Design mismatch (states) in Figma frame \"Checkout / Pay\"",
            "### W8. Optional check \"lint\" failed",
            "## Acceptance criteria status",
            "## Rules to add",
            "## Context",
            "## For coding agents",
        ]
    );
    assert!(markdown.contains("**Verdict: FAILED** - 4 blocking, 8 warnings."));
    assert!(markdown.contains("- required checks failed: unit\n"));
    assert!(markdown.contains("- blocking findings: SQL injection (pay/db.go:42)\n"));
    assert!(markdown.contains("- acceptance criteria not met: AC-2\n"));
    assert!(markdown.contains("- design mismatches: copy:1:2\n"));
    assert!(!markdown.contains("generated tests failed"), "tests only warn under the default policy");
    assert!(markdown.contains("- Dashboard: <http://localhost:8080/#run-42>"));
    assert!(!markdown.contains("Nit"), "info findings are left out");
}

#[test]
fn strict_policy_moves_tests_and_approved_rules_to_blocking() {
    let report = failing_report();
    let rules = BlockingRules { failed_tests: true, rule_violations: true, ..BlockingRules::default() };
    let doc = HandoffDocument::build(&HandoffInput { rules, ..input(&report) });
    let kinds: Vec<TaskKind> = doc.blocking.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TaskKind::FailedCheck,
            TaskKind::Finding,
            TaskKind::FailedTest,
            TaskKind::UnmetCriterion,
            TaskKind::RuleViolation,
            TaskKind::DesignMismatch,
        ]
    );
    assert!(doc.verdict.reasons.iter().any(|r| r == "generated tests failed: S2"));
    assert!(doc.verdict.reasons.iter().any(|r| r == "approved architecture rules violated: R1"));
    let inferred = doc.warnings.iter().find(|t| t.kind == TaskKind::RuleViolation).unwrap();
    assert!(inferred.title.contains("R2") && inferred.facts[0].value.contains("not approved"));
    let lenient = BlockingRules { unmet_criteria: false, ..BlockingRules::default() };
    let doc = HandoffDocument::build(&HandoffInput { rules: lenient, ..input(&report) });
    assert!(doc.warnings.iter().any(|t| t.kind == TaskKind::UnmetCriterion));
}

#[test]
fn failing_test_task_is_self_contained() {
    let report = failing_report();
    let rules = BlockingRules { failed_tests: true, ..BlockingRules::default() };
    let markdown = render_markdown(&HandoffDocument::build(&HandoffInput { rules, ..input(&report) }));
    let start = markdown.find("### 3. Generated test S2 fails").unwrap();
    let end = markdown[start..].find("\n### 4.").unwrap() + start;
    let expected = "### 3. Generated test S2 fails: Total includes tax

- Where: `pay/total.go:10`
- Language: go
- Criterion: SHOP-7/AC-1: Total includes tax
- Given: a cart of 100
- When: the customer checks out
- Then: the total is 110
- Test file: pay/qgate_s2_test.go
- Exit code: 1
- Commit: 3f9c2ab7d1e04c5b8a6f9e2d1c0b3a4f5e6d7c8b

What's wrong:

A test generated from the acceptance criteria fails against this commit.

Test source (pay/qgate_s2_test.go):

```go
func TestS2(t *testing.T) {
\tif Total(100) != 110 { t.Fatal(\"want 110\") }
}
```

Reproduce:

```sh
go test ./pay -run TestS2
```

Failing output:

```text
--- FAIL: TestS2
    want 110, got 100
```

Code under test (pay/total.go:10):

```go
total := net + tax
```

Expected:

the total is 110

Suggested fix:

Change the production code so this test passes. Do not weaken, skip or delete the generated test.
";
    assert_eq!(&markdown[start..end], expected);
}

#[test]
fn advisory_edge_test_is_flagged_as_non_blocking_warning() {
    let mut report = failing_report();
    report.gate.advisory_tests = vec!["S2".into()];
    let doc = HandoffDocument::build(&input(&report));
    assert!(!doc.blocking.iter().any(|t| t.kind == TaskKind::FailedTest));
    let s2 = doc
        .warnings
        .iter()
        .find(|t| t.kind == TaskKind::FailedTest && t.title.contains("S2"))
        .expect("S2 should be a warning under the default policy");
    assert!(
        s2.facts.iter().any(|f| f.label == "Advisory" && f.value.contains("coverage above threshold")),
        "advisory edge test must carry the Advisory fact"
    );
}

#[test]
fn evidence_with_backticks_gets_a_longer_fence() {
    let report = failing_report();
    let markdown = render_markdown(&HandoffDocument::build(&input(&report)));
    let expected = format!("Evidence:\n\n`````go\n{FENCED_EVIDENCE}\n`````\n");
    assert!(markdown.contains(&expected), "{markdown}");
}

#[test]
fn criteria_table_escapes_cells_and_lists_evidence() {
    let report = failing_report();
    let markdown = render_markdown(&HandoffDocument::build(&input(&report)));
    let table = "| ID | Criterion | Status | Evidence |
|---|---|---|---|
| AC-1 | Total includes tax | met | pay/total.go:10 |
| AC-2 | Receipt \\| is emailed | NOT MET | - |
| AC-3 | Works offline | unclear | - |
";
    assert!(markdown.contains(table), "{markdown}");
}

#[test]
fn context_lists_tickets_suggestions_links_and_notes() {
    let report = failing_report();
    let doc = HandoffDocument::build(&input(&report));
    let markdown = render_markdown(&doc);
    let context = &markdown[markdown.find("## Context").unwrap()..markdown.find("## For coding agents").unwrap()];
    assert_eq!(
        context,
        "## Context

- Ticket SHOP-7: Pay with tax (<https://acme.atlassian.net/browse/SHOP-7>)
- Ticket SHOP-1
- Ticket description (summary): As a buyer I want tax included.
- Implicit requirements (suggested, not required):
  - Amounts use integer cents
- Figma links:
  - <https://www.figma.com/design/abc/Shop?node-id=1-2>
- Agent notes:
  - architecture: rulebook rebuilt

"
    );
    assert_eq!(doc.primary_ticket(), Some("SHOP-7"));
    assert!(markdown.contains("5. Commit and reference the ticket key in the commit message (e.g. \"SHOP-7 ...\")"));
}

#[test]
fn passing_run_snapshot() {
    let report = passing_report();
    let mut meta = meta();
    meta.branch = None;
    meta.ticket_keys.clear();
    meta.run_id = None;
    let doc = HandoffDocument::build(&HandoffInput { meta, ..input(&report) });
    assert_eq!(
        render_markdown(&doc),
        "# qgate review: shop-api (detached) 3f9c2ab - PASSED

**Verdict: PASSED** - 0 blocking, 0 warnings.

The quality gate passed.

- Repository: `shop-api`
- Branch: `(detached)`
- Commit: `3f9c2ab7d1e04c5b8a6f9e2d1c0b3a4f5e6d7c8b`
- Reviewed at: 2026-11-20 14:30:05 UTC

## Do this first (blocking)

Nothing blocking.

## Should fix (warnings)

No warnings.

## Acceptance criteria status

No acceptance criteria were checked for this commit.

## Context

- Ticket: none linked to this commit

## For coding agents

1. Fix the items under \"Do this first\" in order; each one is self-contained.
2. Do not weaken, skip or delete the generated tests: they encode the acceptance criteria. They are not in your \
working tree; to reproduce, save the test source at the path shown and run the reproduce command. Change the \
production code, not the assertions.
3. Keep changes minimal and focused on the listed items; then handle the warnings.
4. Re-run the reproduce commands and the failing checks until they pass.
5. Commit and reference the ticket key in the branch name or commit message, so qgate reviews the new commit.
"
    );
    assert!(doc.verdict.reasons.is_empty());
}

#[test]
fn incomplete_review_is_explained() {
    let mut report = passing_report();
    report.gate.incomplete = true;
    report.skipped.push(crate::review::domain::report::SkippedFile {
        path: "big.go".into(),
        reason: crate::review::application::SkipReason::ReviewFailed("overloaded".into()),
    });
    let markdown = render_markdown(&HandoffDocument::build(&input(&report)));
    assert!(markdown.contains("The quality gate passed. Some files could not be reviewed (see Context)."));
    assert!(markdown.contains("  - big.go (review failed: overloaded)"));
    report.gate.passed = false;
    let doc = HandoffDocument::build(&input(&report));
    assert_eq!(doc.verdict.reasons, vec!["review incomplete: some files could not be reviewed".to_string()]);
    report.gate.incomplete = false;
    let doc = HandoffDocument::build(&input(&report));
    assert_eq!(doc.verdict.reasons.len(), 1, "a failed gate always has a reason");
}

#[test]
fn rendering_is_deterministic() {
    let report = failing_report();
    let first = render_markdown(&HandoffDocument::build(&input(&report)));
    let mut shuffled = report.clone();
    let agents = shuffled.agents.as_mut().unwrap();
    agents.tests.reverse();
    agents.violations.reverse();
    agents.design.reverse();
    let second = render_markdown(&HandoffDocument::build(&input(&shuffled)));
    assert_eq!(first, second);
}

#[test]
fn json_has_schema_sections_and_full_report() {
    let report = failing_report();
    let input = input(&report);
    let doc = HandoffDocument::build(&input);
    let value: Value = serde_json::from_str(&render_json(&doc, &input)).unwrap();
    assert_eq!(value["schema"], JSON_SCHEMA);
    assert_eq!(value["meta"]["short_commit"], "3f9c2ab");
    assert_eq!(value["meta"]["run_url"], "http://localhost:8080/#run-42");
    assert_eq!(value["verdict"]["label"], "FAILED");
    assert_eq!(value["blocking"][0]["kind"], "failed_check");
    assert_eq!(value["criteria"][1]["status"], "not_met");
    assert_eq!(value["report"]["gate"]["failed_checks"][0], "unit");
    assert_eq!(value["report"]["agents"]["tests"][0]["scenario_id"], "S2");
    assert_eq!(value["report"]["ticket"]["keys"][0], "SHOP-7");
}

#[test]
fn blocking_rules_follow_the_gate_policy() {
    let policy = crate::review::application::GatePolicy {
        fail_on_test_failure: true,
        fail_on_incomplete: false,
        ..Default::default()
    };
    let rules = BlockingRules::from(&policy);
    assert!(rules.failed_tests && rules.unmet_criteria && !rules.rule_violations && !rules.incomplete);
}

#[test]
fn passing_gate_never_lists_blocking_items_and_long_descriptions_are_cut() {
    let mut report = failing_report();
    report.gate = crate::review::domain::gate::GateDecision {
        passed: true,
        failed_tests: vec!["S2".into()],
        ..Default::default()
    };
    report.checks.retain(|c| !c.required);
    report.findings.remove(0);
    report.ticket.as_mut().unwrap().description = "word ".repeat(500);
    let rules = BlockingRules { failed_tests: true, rule_violations: true, ..BlockingRules::default() };
    let doc = HandoffDocument::build(&HandoffInput { rules, ..input(&report) });
    assert!(doc.blocking.is_empty(), "{:?}", doc.blocking);
    assert!(doc.warnings.iter().any(|t| t.kind == TaskKind::FailedTest));
    assert!(doc.warnings.iter().any(|t| t.kind == TaskKind::UnmetCriterion));
    let description = doc.context.description.unwrap();
    assert!(description.ends_with(" ...") && description.len() < 420);
}

#[test]
fn findings_carry_their_rule_and_the_run_lists_rules_to_add() {
    let report = failing_report();
    let mut doc = HandoffDocument::build(&input(&report));
    let markdown = render_markdown(&doc);
    assert!(markdown.contains("- Rule: Build SQL with bound parameters only. (files: **/*.go)\n"), "{markdown}");
    assert!(markdown.contains("- Rule: Name constants instead of magic numbers.\n"));
    assert!(markdown.contains("Rules from this review; add them to your coding agent's instructions:"));
    assert!(
        markdown.contains(
            "- **[critical] Build SQL with bound parameters only.** — scope: `**/*.go` · from `pay/db.go:42`\n"
        )
    );
    assert!(markdown.contains("- **[minor] R2 title** · from `pay/db.go:3`\n"));

    doc.rules_file = Some("shop-api/qgate-rules.md".into());
    doc.rules_to_add.truncate(1);
    let markdown = render_markdown(&doc);
    assert!(markdown.contains("already added to `shop-api/qgate-rules.md` for your coding agent"));
    let json: Value = serde_json::from_str(&render_json(&doc, &input(&report))).unwrap();
    assert_eq!(json["rules_to_add"][0]["scope"], "**/*.go");
    assert_eq!(json["rules_file"], "shop-api/qgate-rules.md");

    doc.rules_to_add.clear();
    assert!(!render_markdown(&doc).contains("## Rules to add"), "nothing new, no section");
}
