use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::fakes::{FakeGates, FakeHistory, FakeModel, FakeRulebooks, FakeSettings, MemoryProposals};
use super::*;
use crate::policy::domain::dry_run::fixtures::run;
use crate::policy::domain::{
    ApprovalRequest, DraftGate, DraftRule, DraftSetting, ExistingRule, ProposalDraft, ProposalStatus, RuleAction,
    RuleApproval, RunFacts, Sensitivity,
};

const REPO: &str = "/repos/app";

fn fixed_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-21T10:00:00Z").unwrap().with_timezone(&Utc)
}

fn later() -> DateTime<Utc> {
    fixed_now() + chrono::Duration::days(2)
}

fn draft() -> ProposalDraft {
    ProposalDraft {
        summary: "Block on failing generated tests".into(),
        settings_changes: vec![
            DraftSetting { path: "agents.tests.on_fail".into(), value_json: "\"block\"".into(), reason: "r".into() },
            DraftSetting { path: "watch.run_repo_checks".into(), value_json: "false".into(), reason: "r".into() },
            DraftSetting { path: "agents.budget_usd_per_review".into(), value_json: "99".into(), reason: "r".into() },
        ],
        rule_changes: vec![DraftRule {
            action: RuleAction::Add,
            title: "No SQL in handlers".into(),
            description: "Handlers call repositories.".into(),
            scope_globs: vec!["src/api/**".into()],
            ..DraftRule::default()
        }],
        gate_changes: vec![DraftGate {
            field: "fail_on_incomplete".into(),
            value_json: "false".into(),
            reason: "flaky API".into(),
        }],
        ..ProposalDraft::default()
    }
}

struct Harness {
    model: Arc<FakeModel>,
    settings: Arc<FakeSettings>,
    rulebooks: Arc<FakeRulebooks>,
    store: Arc<MemoryProposals>,
    service: PolicyService,
}

fn harness_with(
    model: FakeModel,
    history: FakeHistory,
    rulebooks: FakeRulebooks,
    now: fn() -> DateTime<Utc>,
) -> Harness {
    let model = Arc::new(model);
    let settings = Arc::new(FakeSettings::default());
    let rulebooks = Arc::new(rulebooks);
    let store = Arc::new(MemoryProposals::default());
    let service = PolicyService::new(PolicyDeps {
        model: model.clone(),
        settings: settings.clone(),
        rulebooks: rulebooks.clone(),
        history: Arc::new(history),
        gates: Arc::new(FakeGates::default()),
        store: store.clone(),
        now,
    });
    Harness { model, settings, rulebooks, store, service }
}

fn known_rulebooks() -> FakeRulebooks {
    let rule = ExistingRule {
        id: "R1".into(),
        title: "Domain is pure".into(),
        description: "d".into(),
        status: "approved".into(),
        scope_globs: vec![],
    };
    FakeRulebooks {
        rules: std::sync::Mutex::new(HashMap::from([(REPO.to_string(), vec![rule])])),
        ..FakeRulebooks::default()
    }
}

fn harness() -> Harness {
    harness_with(FakeModel::returning(draft()), FakeHistory::default(), known_rulebooks(), fixed_now)
}

#[tokio::test]
async fn propose_validates_the_model_draft_and_stores_a_draft() {
    let h = harness();
    let view = h.service.propose("  Block when generated tests fail.  ", Some(REPO.into())).await.unwrap();
    let p = &view.proposal;
    assert_eq!((p.id, p.status, p.settings_version), (1, ProposalStatus::Draft, 3));
    assert_eq!(p.description, "Block when generated tests fail.");
    let paths: Vec<&str> = p.content.settings.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["agents.tests.on_fail", "watch.run_repo_checks"]);
    assert_eq!(p.content.settings[1].sensitivity, vec![Sensitivity::RepoChecks, Sensitivity::GateLoosening]);
    assert!(p.content.rejected.iter().any(|r| r.subject == "agents.budget_usd_per_review" && r.reason.contains("50")));
    assert_eq!(p.content.rules.len(), 1);
    assert!(p.content.gate[0].loosening);
    assert!(view.repo_patch.as_deref().unwrap().contains("+fail_on_incomplete = false"));
    assert_eq!(view.preview.len(), 3);
    assert!(!view.stale && !view.expired);

    let prompt = h.model.prompts.lock().unwrap()[0].clone();
    assert_eq!(prompt.repo.as_deref(), Some(REPO));
    assert_eq!(prompt.rules[0].id, "R1");
    assert!(prompt.settings.iter().any(|(path, _)| path == "agents.tests.on_fail"));
    assert!(prompt.settings.iter().all(|(path, _)| !path.starts_with("dashboard.")), "only settable fields");
    assert_eq!(h.store.rows.lock().unwrap().len(), 1);
    assert!(h.settings.applied.lock().unwrap().is_empty(), "proposing never applies anything");
}

#[tokio::test]
async fn propose_rejects_bad_input_unknown_repos_and_reports_model_errors() {
    let h = harness();
    assert!(matches!(h.service.propose("   ", None).await, Err(PolicyError::Invalid(_))));
    let long = "x".repeat(8_001);
    assert!(matches!(h.service.propose(&long, None).await, Err(PolicyError::Invalid(_))));
    let unknown = h.service.propose("x", Some("/etc".into())).await;
    assert!(matches!(unknown, Err(PolicyError::Invalid(m)) if m.contains("unknown repository")));

    let model = FakeModel { error: Some(PolicyError::ModelUnavailable("no key".into())), ..FakeModel::default() };
    let h = harness_with(model, FakeHistory::default(), FakeRulebooks::default(), fixed_now);
    assert_eq!(
        h.service.propose("x", Some(" ".into())).await.unwrap_err(),
        PolicyError::ModelUnavailable("no key".into())
    );
}

#[tokio::test]
async fn combined_settings_that_conflict_are_all_rejected() {
    let conflicting = ProposalDraft {
        settings_changes: vec![
            DraftSetting { path: "agents.tests.on_fail".into(), value_json: "\"block\"".into(), reason: String::new() },
            DraftSetting { path: "agents.tests.enabled".into(), value_json: "false".into(), reason: String::new() },
        ],
        ..ProposalDraft::default()
    };
    let h =
        harness_with(FakeModel::returning(conflicting), FakeHistory::default(), FakeRulebooks::default(), fixed_now);
    let view = h.service.propose("x", None).await.unwrap();
    assert!(view.proposal.content.settings.is_empty());
    assert!(view.proposal.content.rejected.iter().all(|r| r.reason.contains("conflicts")));
}

fn approve_all(confirm: bool) -> ApprovalRequest {
    ApprovalRequest { settings: vec![0, 1], rules: vec![(0, RuleApproval::Approved)], confirm_sensitive: confirm }
}

#[tokio::test]
async fn approve_applies_settings_as_a_version_and_rules_then_refuses_a_second_time() {
    let h = harness();
    let id = h.service.propose("x", Some(REPO.into())).await.unwrap().proposal.id;
    let refused = h.service.approve(id, approve_all(false)).await.unwrap_err();
    assert!(matches!(refused, PolicyError::NeedsConfirmation(m) if m.contains("watch.run_repo_checks")));
    assert!(h.settings.applied.lock().unwrap().is_empty());

    let view = h.service.approve(id, approve_all(true)).await.unwrap();
    assert_eq!(view.proposal.status, ProposalStatus::Applied);
    let outcome = view.proposal.outcome.clone().unwrap();
    assert_eq!(outcome.settings_version, Some(4));
    assert_eq!(outcome.rules_written, vec!["No SQL in handlers"]);
    let applied = h.settings.applied.lock().unwrap().clone();
    assert_eq!(applied[0].1, "policy-assistant:p1");
    let writes = h.rulebooks.writes.lock().unwrap().clone();
    assert_eq!(writes[0].0, REPO);
    assert_eq!(writes[0].1[0].status, RuleApproval::Approved);
    assert_eq!(h.store.get(id).unwrap().unwrap().status, ProposalStatus::Applied);
    assert!(matches!(h.service.approve(id, approve_all(true)).await, Err(PolicyError::Conflict(_))));
    assert!(matches!(h.service.repreview(id).await, Err(PolicyError::Conflict(_))));
}

#[tokio::test]
async fn stale_proposals_are_refused_and_can_be_re_previewed() {
    let h = harness();
    let id = h.service.propose("x", Some(REPO.into())).await.unwrap().proposal.id;
    h.settings.bump_version();
    assert!(h.service.get(id).await.unwrap().stale);
    let refused = h.service.approve(id, approve_all(true)).await.unwrap_err();
    assert!(matches!(refused, PolicyError::Conflict(m) if m.contains("re-preview")));
    assert!(h.settings.applied.lock().unwrap().is_empty());

    let fresh = h.service.repreview(id).await.unwrap();
    assert_eq!((fresh.proposal.id, fresh.proposal.settings_version), (2, 4));
    assert_eq!(h.model.prompts.lock().unwrap().len(), 1, "re-preview does not call the model again");
    assert_eq!(h.store.get(id).unwrap().unwrap().status, ProposalStatus::Rejected);
    assert!(h.service.approve(2, approve_all(true)).await.is_ok());
}

#[tokio::test]
async fn expired_proposals_and_rejections() {
    let h = harness_with(FakeModel::returning(draft()), FakeHistory::default(), known_rulebooks(), later);
    let id = h.service.propose("x", Some(REPO.into())).await.unwrap().proposal.id;
    let mut stored = h.store.get(id).unwrap().unwrap();
    stored.expires_at = fixed_now();
    h.store.rows.lock().unwrap()[0] = stored;
    assert!(h.service.get(id).await.unwrap().expired);
    assert!(
        matches!(h.service.approve(id, approve_all(true)).await, Err(PolicyError::Conflict(m)) if m.contains("expired"))
    );
    let rejected = h.service.reject(id).await.unwrap();
    assert_eq!(rejected.proposal.status, ProposalStatus::Rejected);
    assert!(matches!(h.service.reject(id).await, Err(PolicyError::Conflict(_))));
    assert!(matches!(h.service.get(99).await, Err(PolicyError::NotFound(_))));
    assert_eq!(h.service.recent().await.unwrap()[0].status, ProposalStatus::Rejected);
}

#[tokio::test]
async fn partial_failures_are_recorded_and_nothing_selected_is_invalid() {
    let rulebooks = FakeRulebooks { fail: true, ..known_rulebooks() };
    let h = harness_with(FakeModel::returning(draft()), FakeHistory::default(), rulebooks, fixed_now);
    let id = h.service.propose("x", Some(REPO.into())).await.unwrap().proposal.id;
    let empty = h.service.approve(id, ApprovalRequest::default()).await.unwrap_err();
    assert!(matches!(empty, PolicyError::Invalid(_)));
    let rules_only = ApprovalRequest { rules: vec![(0, RuleApproval::Proposed)], ..ApprovalRequest::default() };
    assert!(matches!(h.service.approve(id, rules_only).await, Err(PolicyError::Conflict(_))));
    let view = h.service.approve(id, approve_all(true)).await.unwrap();
    let outcome = view.proposal.outcome.unwrap();
    assert_eq!(outcome.settings_version, Some(4));
    assert!(outcome.errors[0].contains("rules were not"));
}

#[tokio::test]
async fn dry_run_replays_history_with_the_proposed_policy() {
    let failing_test = RunFacts { failed_tests: 1, repo_path: REPO.into(), ..run(1, "app") };
    let incomplete = RunFacts { incomplete: true, recorded_passed: false, repo_path: REPO.into(), ..run(2, "app") };
    let other_repo = RunFacts { incomplete: true, recorded_passed: false, ..run(3, "other") };
    let history =
        FakeHistory { runs: vec![failing_test, incomplete, other_repo], skipped: 1, ..FakeHistory::default() };
    let h = harness_with(FakeModel::returning(draft()), history, known_rulebooks(), fixed_now);
    let id = h.service.propose("x", Some(REPO.into())).await.unwrap().proposal.id;
    let report = h.service.dry_run(id, Some(500)).await.unwrap();
    assert_eq!((report.evaluated, report.skipped), (3, 1));
    assert_eq!((report.pass_to_fail, report.fail_to_pass), (1, 1), "tests now block; incomplete passes only in app");
    assert_eq!(report.flips.iter().map(|f| f.run_id).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(report.caveats.len(), 2, "new rules cannot be replayed");
    assert!(matches!(h.service.dry_run(42, None).await, Err(PolicyError::NotFound(_))));
}

#[tokio::test]
async fn prefill_describes_a_stored_finding_and_lists_known_repos() {
    let finding = FindingContext {
        repo_path: REPO.into(),
        file: "src/api/orders.rs".into(),
        line: 12,
        severity: "major".into(),
        category: "security".into(),
        title: "SQL built from user input".into(),
        explanation: "Injection.".into(),
        evidence: "format!(\"SELECT {id}\")".into(),
    };
    let history = FakeHistory {
        runs: vec![RunFacts { repo_path: "/repos/other".into(), ..run(1, "other") }],
        findings: HashMap::from([((7, 0), finding)]),
        ..FakeHistory::default()
    };
    let h = harness_with(FakeModel::default(), history, known_rulebooks(), fixed_now);
    let prefill = h.service.prefill(7, 0).await.unwrap();
    assert_eq!(prefill.repo, REPO);
    assert!(prefill.description.contains("[major / security] SQL built from user input at src/api/orders.rs:12"));
    assert!(prefill.description.contains("Injection.") && prefill.description.contains("SELECT"));
    assert!(matches!(h.service.prefill(7, 1).await, Err(PolicyError::NotFound(_))));
    assert_eq!(h.service.repos().await.unwrap(), vec!["/repos/app".to_string(), "/repos/other".to_string()]);
}
