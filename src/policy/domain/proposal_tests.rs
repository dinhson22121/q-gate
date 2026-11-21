use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use super::*;

fn settings() -> Value {
    json!({
        "agents": {"enabled": true, "budget_usd_per_review": 2.0, "tests": {"on_fail": "block"}},
        "llm": {"claude_bin": "claude", "timeout_secs": 600},
        "notify": {"webhook_url": ""},
        "watch": {"run_repo_checks": true},
        "jira": {"base_url": "https://acme.atlassian.net", "mcp": {"command": ["uvx"]}},
        "output": {"dir": "/data/reviews", "enabled": false},
        "dashboard": {"bind": "127.0.0.1:8080"}
    })
}

fn rules() -> Vec<ExistingRule> {
    vec![ExistingRule {
        id: "R1".into(),
        title: "Domain is pure".into(),
        description: "d".into(),
        status: "approved".into(),
        scope_globs: vec![],
    }]
}

fn setting(path: &str, value: &str) -> DraftSetting {
    DraftSetting { path: path.into(), value_json: value.into(), reason: "because".into() }
}

fn context<'a>(settings: &'a Value, rules: &'a [ExistingRule], repo: Option<&'a str>) -> ProposalContext<'a> {
    ProposalContext {
        settings,
        locked: LOCKED,
        restart_required: RESTART,
        repo,
        rules,
        repo_gate: None,
        existing_checks: CHECKS,
    }
}

const LOCKED: &[String] = &[];
const RESTART: &[String] = &[];
const CHECKS: &[String] = &[];

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-21T10:00:00Z").unwrap().with_timezone(&Utc)
}

#[test]
fn settings_are_allow_listed_typed_and_flagged() {
    let settings = settings();
    let draft = ProposalDraft {
        settings_changes: vec![
            setting("agents.tests.on_fail", "\"warn\""),
            setting("agents.budget_usd_per_review", "5"),
            setting("llm.claude_bin", "\"/tmp/evil\""),
            setting("secrets.jira_api_token", "\"sk\""),
            setting("dashboard.bind", "\"0.0.0.0:1\""),
            setting("agents.enabled", "\"yes\""),
            setting("agents.enabled", "true"),
            setting("output.enabled", "not json"),
            setting("output.missing", "1"),
            setting("agents.budget_usd_per_review", "3"),
            setting("notify.webhook_url", "\"https://hooks.example/t\""),
            setting("notify.webhook_url", "\"\""),
            setting("jira.mcp.command", "[\"sh\"]"),
        ],
        ..ProposalDraft::default()
    };
    let content = validate_draft(&draft, &context(&settings, &[], None));
    let paths: Vec<&str> = content.settings.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["agents.tests.on_fail", "agents.budget_usd_per_review", "notify.webhook_url"]);
    assert_eq!(content.settings[0].sensitivity, vec![Sensitivity::GateLoosening]);
    assert!(!content.settings[1].is_sensitive());
    assert_eq!(content.settings[2].sensitivity, vec![Sensitivity::Webhook]);
    assert_eq!(content.settings[2].old, Value::Null, "the webhook URL is never echoed");
    let reasons: Vec<(&str, &str)> = content.rejected.iter().map(|r| (r.subject.as_str(), r.reason.as_str())).collect();
    assert!(reasons.iter().any(|(s, r)| *s == "secrets.jira_api_token" && r.contains("credentials")));
    assert!(reasons.iter().any(|(s, r)| *s == "dashboard.bind" && r.contains("cannot be changed")));
    assert!(reasons.iter().any(|(s, r)| *s == "agents.enabled" && r.contains("expected")));
    assert!(reasons.iter().any(|(s, r)| *s == "output.enabled" && r.contains("JSON")));
    assert!(reasons.iter().any(|(s, r)| *s == "output.missing" && r.contains("unknown")));
    assert!(reasons.iter().any(|(_, r)| r.contains("twice")));
    assert!(reasons.iter().any(|(s, r)| *s == "notify.webhook_url" && r.contains("Settings page")));
    for admin in ["llm.claude_bin", "jira.mcp.command"] {
        assert!(reasons.iter().any(|(s, r)| *s == admin && r.starts_with("admin-only")), "{admin}");
    }
    assert_eq!(content.rejected.len(), 9, "unchanged agents.enabled=true is silently dropped");
}

#[test]
fn repository_checks_can_only_be_turned_off() {
    let mut settings = settings();
    let draft = |value: &str| ProposalDraft {
        settings_changes: vec![setting("watch.run_repo_checks", value)],
        ..ProposalDraft::default()
    };
    let off = validate_draft(&draft("false"), &context(&settings, &[], None));
    assert_eq!(off.settings[0].sensitivity, vec![Sensitivity::RepoChecks, Sensitivity::GateLoosening]);
    settings["watch"]["run_repo_checks"] = json!(false);
    let on = validate_draft(&draft("true"), &context(&settings, &[], None));
    assert!(on.settings.is_empty());
    assert!(on.rejected[0].reason.starts_with("admin-only"));
}

#[test]
fn locked_and_restart_fields_are_reported() {
    let settings = settings();
    let locked = vec!["output.dir".to_string()];
    let restart = vec!["output.enabled".to_string()];
    let ctx = ProposalContext { locked: &locked, restart_required: &restart, ..context(&settings, &[], None) };
    let draft = ProposalDraft {
        settings_changes: vec![setting("output.dir", "\"/x\""), setting("output.enabled", "true")],
        ..ProposalDraft::default()
    };
    let content = validate_draft(&draft, &ctx);
    assert_eq!(content.rejected[0].subject, "output.dir");
    assert!(content.rejected[0].reason.contains("environment variable"));
    assert!(content.settings[0].restart_required);
}

fn rule(action: RuleAction, id: &str, title: &str, globs: &[&str]) -> DraftRule {
    DraftRule {
        action,
        rule_id: id.into(),
        title: title.into(),
        description: "desc".into(),
        scope_globs: globs.iter().map(|g| g.to_string()).collect(),
        reason: String::new(),
    }
}

#[test]
fn repository_items_need_a_repository_and_are_validated() {
    let settings = settings();
    let rules = rules();
    let draft = ProposalDraft {
        rule_changes: vec![
            rule(RuleAction::Add, "", "No SQL in handlers", &["src/api/**"]),
            rule(RuleAction::Edit, "R1", "Domain stays pure", &[]),
            rule(RuleAction::Edit, "R9", "Ghost", &[]),
            rule(RuleAction::Add, "", "Bad glob", &["src/[**"]),
            rule(RuleAction::Add, "", "", &[]),
            rule(RuleAction::Add, "", "no sql in HANDLERS", &[]),
            rule(RuleAction::Add, "", &"x".repeat(200), &[]),
        ],
        gate_changes: vec![
            DraftGate { field: "fail_on".into(), value_json: "\"major\"".into(), reason: String::new() },
            DraftGate { field: "fail_on_incomplete".into(), value_json: "false".into(), reason: String::new() },
            DraftGate { field: "fail_on_unmet_criteria".into(), value_json: "true".into(), reason: String::new() },
            DraftGate { field: "fail_on".into(), value_json: "\"info\"".into(), reason: String::new() },
            DraftGate { field: "bogus".into(), value_json: "1".into(), reason: String::new() },
            DraftGate { field: "min_confidence".into(), value_json: "nope".into(), reason: String::new() },
        ],
        check_suggestions: vec![
            DraftCheck {
                name: "lint".into(),
                command: vec!["cargo".into(), "clippy".into()],
                required: true,
                timeout_secs: 600,
                reason: String::new(),
            },
            DraftCheck {
                name: "tests".into(),
                command: vec!["go".into()],
                required: true,
                timeout_secs: 60,
                ..Default::default()
            },
            DraftCheck { name: "lint".into(), command: vec!["x".into()], timeout_secs: 1, ..Default::default() },
            DraftCheck { name: "empty".into(), command: vec![], timeout_secs: 1, ..Default::default() },
            DraftCheck { name: "slow".into(), command: vec!["x".into()], timeout_secs: 0, ..Default::default() },
            DraftCheck { name: " ".into(), command: vec!["x".into()], timeout_secs: 1, ..Default::default() },
        ],
        ..ProposalDraft::default()
    };
    let without_repo = validate_draft(&draft, &context(&settings, &rules, None));
    assert!(without_repo.is_empty());
    assert_eq!(without_repo.rejected.len(), 19);

    let existing = vec!["tests".to_string()];
    let ctx = ProposalContext { existing_checks: &existing, ..context(&settings, &rules, Some("/repos/app")) };
    let content = validate_draft(&draft, &ctx);
    assert_eq!(content.rules.len(), 2);
    assert_eq!(content.rules[1].replaces, Some(("R1".to_string(), "Domain is pure".to_string())));
    assert_eq!(content.gate.len(), 2, "unchanged fail_on_unmet_criteria is dropped; fail_on twice is rejected");
    assert_eq!(content.gate[0].new, json!("major"));
    assert!(!content.gate[0].loosening);
    assert!(content.gate[1].loosening);
    assert_eq!(content.checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["lint"]);
    assert_eq!(content.rejected.len(), 5 + 3 + 5);
    assert!(!content.is_empty());
}

#[test]
fn oversized_lists_and_texts_are_capped() {
    let settings = settings();
    let draft = ProposalDraft {
        summary: "s".repeat(5000),
        settings_changes: (0..25).map(|_| setting("dashboard.bind", "1")).collect(),
        unsupported: (0..30).map(|i| format!("u{i}")).collect(),
        ..ProposalDraft::default()
    };
    let content = validate_draft(&draft, &context(&settings, &[], None));
    assert!(content.summary.chars().count() <= 2001);
    assert_eq!(content.unsupported.len(), MAX_ITEMS);
    assert!(content.rejected.iter().any(|r| r.subject == "5 more"));
}

fn proposal() -> Proposal {
    let settings = settings();
    let draft = ProposalDraft {
        settings_changes: vec![
            setting("agents.tests.on_fail", "\"warn\""),
            setting("agents.budget_usd_per_review", "5"),
        ],
        rule_changes: vec![rule(RuleAction::Add, "", "No SQL in handlers", &[])],
        ..ProposalDraft::default()
    };
    let content = validate_draft(&draft, &context(&settings, &[], Some("/repos/app")));
    Proposal::new("desc".into(), Some("/repos/app".into()), 7, draft, content, now())
}

#[test]
fn approval_checks_lifecycle_staleness_selection_and_confirmation() {
    let p = proposal();
    let request = |settings: Vec<usize>, confirm| ApprovalRequest {
        settings,
        rules: vec![(0, RuleApproval::Approved)],
        confirm_sensitive: confirm,
    };
    let plan = p.plan(&request(vec![1, 1], false), 7, now()).unwrap();
    assert_eq!(plan.settings.len(), 1);
    assert_eq!(plan.rules[0].1, RuleApproval::Approved);
    assert_eq!(
        p.plan(&request(vec![0], false), 7, now()).unwrap_err(),
        ApprovalError::NeedsConfirmation("agents.tests.on_fail".into())
    );
    assert_eq!(p.plan(&request(vec![0], true), 7, now()).unwrap().settings.len(), 1);
    assert_eq!(
        p.plan(&request(vec![], false), 8, now()).unwrap_err(),
        ApprovalError::Stale { proposed: 7, current: 8 }
    );
    assert!(p.plan(&request(vec![], false), 8, now()).unwrap_err().to_string().contains("re-preview"));
    assert_eq!(p.plan(&request(vec![5], false), 7, now()).unwrap_err(), ApprovalError::UnknownItem("setting", 5));
    let bad_rule = ApprovalRequest { rules: vec![(3, RuleApproval::Proposed)], ..ApprovalRequest::default() };
    assert_eq!(p.plan(&bad_rule, 7, now()).unwrap_err(), ApprovalError::UnknownItem("rule", 3));
    assert_eq!(p.plan(&ApprovalRequest::default(), 7, now()).unwrap_err(), ApprovalError::NothingSelected);
    let later = now() + Duration::hours(25);
    assert!(p.is_expired(later));
    assert_eq!(p.plan(&request(vec![], false), 7, later).unwrap_err(), ApprovalError::Expired);
    let applied = Proposal { status: ProposalStatus::Applied, ..p };
    assert_eq!(applied.plan(&request(vec![], false), 7, now()).unwrap_err(), ApprovalError::NotDraft("applied"));
}

#[test]
fn status_strings_round_trip_and_text_is_clipped() {
    for status in [ProposalStatus::Draft, ProposalStatus::Applied, ProposalStatus::Rejected] {
        assert_eq!(ProposalStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(ProposalStatus::parse("x"), None);
    assert_eq!(clip("  abcdef ", 3), "abc…");
    assert_eq!(clip("ab", 3), "ab");
}
