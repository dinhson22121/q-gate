use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::Utc;

use super::fakes::{FakeModel, MemoryRepo, MemoryStore};
use super::*;
use crate::conventions::domain::rulebook::{FileLine, RuleDraft, RuleKind, RuleStatus, Rulebook};
use crate::review::application::TicketBrief;
use crate::review::application::agent_ports::{AgentInput, ArchitectureReviewer};
use crate::review::domain::change_set::FileChange;
use crate::review::domain::change_set::fixtures::{added, context, file};

const CLAUDE_MD: &str = "# Rules\nThe domain layer must never import infrastructure code.\n";
const DOMAIN_RS: &str =
    "use crate::domain::order::Order;\nuse thiserror::Error;\n\npub fn total(o: &Order) -> u64 { 0 }\n";

fn repo() -> MemoryRepo {
    MemoryRepo::with(&[
        ("CLAUDE.md", CLAUDE_MD),
        ("pyproject.toml", "[project]\nname = 'x'\n"),
        ("src/domain/order.rs", DOMAIN_RS),
        ("README.md", "hello"),
    ])
}

fn example(file: &str, line: u32, quote: &str) -> FileLine {
    FileLine { file: file.into(), line, quote: quote.into() }
}

fn rule(title: &str, kind: RuleKind, globs: &[&str], examples: Vec<FileLine>) -> RuleDraft {
    RuleDraft {
        title: title.into(),
        description: format!("{title}."),
        kind,
        scope_globs: globs.iter().map(|g| g.to_string()).collect(),
        examples,
        lint_suggestion: None,
    }
}

fn standard_drafts() -> Vec<RuleDraft> {
    vec![
        rule(
            "Domain never imports infrastructure",
            RuleKind::Explicit,
            &["src/**"],
            vec![example("CLAUDE.md", 2, "The domain layer must never import infrastructure code.")],
        ),
        rule(
            "Errors use thiserror",
            RuleKind::Inferred,
            &[],
            vec![example("src/domain/order.rs", 2, "use thiserror::Error;")],
        ),
        rule(
            "Invented rule",
            RuleKind::Inferred,
            &[],
            vec![example("src/domain/order.rs", 1, "use diesel::prelude::*;")],
        ),
    ]
}

struct Harness {
    model: Arc<FakeModel>,
    repo: Arc<MemoryRepo>,
    store: Arc<MemoryStore>,
    service: Arc<RulebookService>,
    root: PathBuf,
}

fn harness(name: &str, model: FakeModel) -> Harness {
    let model = Arc::new(model);
    let repo = Arc::new(repo());
    let store = Arc::new(MemoryStore::default());
    let service = Arc::new(RulebookService::new(model.clone(), repo.clone(), store.clone()));
    let root = PathBuf::from(format!("/repos/app-{name}"));
    Harness { model, repo, store, service, root }
}

fn standard(name: &str) -> Harness {
    harness(name, FakeModel::drafting(standard_drafts()))
}

#[tokio::test]
async fn build_keeps_grounded_rules_and_sends_sources_and_samples() {
    let h = standard("build");
    let book = h.service.current(&h.root, 7).await.unwrap();
    assert_eq!(book.repo, repo_key(&h.root));
    let titles: Vec<&str> = book.rules.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, ["Domain never imports infrastructure", "Errors use thiserror"]);
    assert_eq!(book.rules[0].status, RuleStatus::Approved);
    assert_eq!(book.rules[1].status, RuleStatus::Proposed);
    let request = h.model.last_draft.lock().unwrap().clone().unwrap();
    let sources: Vec<&str> = request.sources.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(sources, ["CLAUDE.md"], "pyproject without [tool.ruff] is not a source");
    assert_eq!(request.samples[0].path, "src/domain/order.rs");
    assert_eq!(request.layout, ["src", "src/domain"]);
    assert_eq!(request.languages, [("rust".to_string(), 1)]);
    assert!(h.store.load(&book.repo).unwrap().is_some());
}

#[tokio::test]
async fn fresh_rulebook_is_reused_and_rebuilt_when_sources_change_or_it_ages() {
    let h = standard("staleness");
    h.service.current(&h.root, 7).await.unwrap();
    h.service.current(&h.root, 7).await.unwrap();
    assert_eq!(h.model.draft_calls(), 1);

    h.repo.set("src/domain/order.rs", "changed code does not matter\n");
    h.service.current(&h.root, 7).await.unwrap();
    assert_eq!(h.model.draft_calls(), 1, "code outside rule sources and layout does not trigger a rebuild");

    h.repo.set("CLAUDE.md", "# Rules\nNew rule text.\n");
    h.service.current(&h.root, 7).await.unwrap();
    assert_eq!(h.model.draft_calls(), 2);

    let key = repo_key(&h.root);
    let old = Rulebook { created_at: Utc::now() - chrono::Duration::days(8), ..h.store.load(&key).unwrap().unwrap() };
    h.store.save(&old).unwrap();
    h.service.current(&h.root, 7).await.unwrap();
    assert_eq!(h.model.draft_calls(), 3);
}

#[tokio::test]
async fn decisions_survive_a_rebuild() {
    let h = standard("carry-over");
    let key = repo_key(&h.root);
    h.service.current(&h.root, 7).await.unwrap();
    h.service.decide(&key, "R2", RuleStatus::Approved).await.unwrap();
    h.service.decide(&key, "R1", RuleStatus::Rejected).await.unwrap();
    // The model returns the rules in another order with a differently formatted title.
    let mut drafts = standard_drafts();
    drafts.swap(0, 1);
    drafts[0].title = "errors use THISERROR.".into();
    *h.model.drafts.lock().unwrap() = drafts;
    let book = h.service.rebuild(&h.root).await.unwrap();
    assert_eq!(book.rules[0].id, "R1");
    assert_eq!(book.rules[0].title, "errors use THISERROR.");
    assert_eq!(book.rules[0].status, RuleStatus::Approved);
    assert_eq!(book.rules[1].status, RuleStatus::Rejected);
}

fn team_rule(title: &str, replaces: Option<RuleRef>, status: RuleStatus) -> TeamRule {
    TeamRule { replaces, title: title.into(), description: "d".into(), scope_globs: vec![], status }
}

#[tokio::test]
async fn team_rules_create_a_rulebook_and_survive_rebuilds_with_their_status() {
    let h = standard("team");
    let key = repo_key(&h.root);
    let created = h
        .service
        .add_team_rules(&key, vec![team_rule("No SQL in handlers", None, RuleStatus::Proposed)])
        .await
        .unwrap();
    assert_eq!(created.rules.len(), 1);
    assert!(created.fingerprint.is_empty(), "a placeholder rulebook is rebuilt on the next review");

    let book = h.service.current(&h.root, 7).await.unwrap();
    assert_eq!(h.model.draft_calls(), 1);
    let titles: Vec<&str> = book.rules.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, ["No SQL in handlers", "Domain never imports infrastructure", "Errors use thiserror"]);
    assert_eq!((book.rules[0].kind, book.rules[0].status), (RuleKind::Team, RuleStatus::Proposed));

    let edit = RuleRef { id: "R3".into(), title: "Errors use thiserror".into() };
    let edited = h
        .service
        .add_team_rules(&key, vec![team_rule("Errors use thiserror with context", Some(edit), RuleStatus::Approved)])
        .await
        .unwrap();
    assert_eq!(edited.rules[2].title, "Errors use thiserror with context");
    let rebuilt = h.service.rebuild(&h.root).await.unwrap();
    let status_of = |title: &str| rebuilt.rules.iter().find(|r| r.title == title).map(|r| r.status);
    assert_eq!(status_of("Errors use thiserror"), Some(RuleStatus::Rejected), "the edited rule stays replaced");
    assert_eq!(status_of("Errors use thiserror with context"), Some(RuleStatus::Approved));

    let stale = RuleRef { id: "R1".into(), title: "Something else".into() };
    let refused = h.service.add_team_rules(&key, vec![team_rule("x", Some(stale), RuleStatus::Approved)]).await;
    assert!(matches!(refused, Err(ConventionsError::Conflict(_))));
}

#[tokio::test]
async fn decide_rejects_unknown_rules_and_repos() {
    let h = standard("decide");
    let key = repo_key(&h.root);
    assert!(matches!(h.service.decide(&key, "R1", RuleStatus::Approved).await, Err(ConventionsError::NotFound(_))));
    h.service.current(&h.root, 7).await.unwrap();
    assert!(matches!(h.service.decide(&key, "R9", RuleStatus::Approved).await, Err(ConventionsError::NotFound(_))));
    assert_eq!(h.service.repos().await.unwrap(), [key]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reviews_share_one_build() {
    let model = FakeModel { delay: Some(Duration::from_millis(100)), ..FakeModel::drafting(standard_drafts()) };
    let h = harness("concurrency", model);
    // A second service instance (e.g. the dashboard's) shares the same per-repo lock.
    let other = Arc::new(RulebookService::new(h.model.clone(), h.repo.clone(), h.store.clone()));
    let calls = (0..6).map(|i| {
        let service = if i % 2 == 0 { h.service.clone() } else { other.clone() };
        let root = h.root.clone();
        tokio::spawn(async move { service.current(&root, 7).await })
    });
    for result in futures::future::join_all(calls).await {
        assert_eq!(result.unwrap().unwrap().rules.len(), 2);
    }
    assert_eq!(h.model.draft_calls(), 1);
}

#[tokio::test]
async fn background_rebuild_only_for_known_repos_and_records_failures() {
    let h = harness("background", FakeModel { fail: true, ..FakeModel::default() });
    let key = repo_key(&h.root);
    assert!(matches!(h.service.start_rebuild(&key).await, Err(ConventionsError::NotFound(_))));
    assert!(h.service.current(&h.root, 7).await.is_err());
    assert_eq!(h.service.build_state(&key).last_error.as_deref(), Some("rule model failed: model down"));

    h.store
        .save(&Rulebook { repo: key.clone(), fingerprint: "x".into(), rules: Vec::new(), created_at: Utc::now() })
        .unwrap();
    h.service.start_rebuild(&key).await.unwrap();
    for _ in 0..100 {
        if !h.service.build_state(&key).building {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(h.model.draft_calls(), 2);
    assert!(h.service.build_state(&key).last_error.is_some());
}

// ---------- reviewer ----------

fn change() -> FileChange {
    file(
        "src/domain/order.rs",
        1,
        vec![context(1, "use crate::domain::order::Order;"), added(2, "use crate::infra::db::Pool;")],
        Some("use crate::domain::order::Order;\nuse crate::infra::db::Pool;\n"),
    )
}

fn input<'a>(files: &'a [FileChange], root: &'a Path) -> AgentInput<'a> {
    AgentInput { files, requirements: None, ticket: None, commit: "abc", repo_root: root, snapshot_root: None }
}

fn violation(rule_id: &str, evidence: &str) -> ViolationDraft {
    ViolationDraft {
        rule_id: rule_id.into(),
        file: "./src/domain/order.rs".into(),
        line: 2,
        evidence: evidence.into(),
        explanation: "imports infrastructure".into(),
        suggestion: Some(" ".into()),
    }
}

async fn reviewer_with_rejected(name: &str, use_inferred: bool) -> (Harness, RulebookReviewer) {
    let mut drafts = standard_drafts();
    drafts.push(rule(
        "Rejected rule",
        RuleKind::Explicit,
        &[],
        vec![example("CLAUDE.md", 1, "# Rules\nThe domain layer")],
    ));
    drafts.push(rule(
        "Only web code",
        RuleKind::Explicit,
        &["web/**"],
        vec![example("CLAUDE.md", 2, "must never import")],
    ));
    let h = harness(name, FakeModel::drafting(drafts));
    let key = repo_key(&h.root);
    h.service.current(&h.root, 7).await.unwrap();
    h.service.decide(&key, "R3", RuleStatus::Rejected).await.unwrap();
    let reviewer = RulebookReviewer::with_service(h.service.clone(), use_inferred, 7);
    (h, reviewer)
}

#[tokio::test]
async fn reviewer_grounds_violations_against_selected_rules_and_new_code() {
    let (h, reviewer) = reviewer_with_rejected("review", true).await;
    *h.model.violations.lock().unwrap() = vec![
        violation("R1", "use crate::infra::db::Pool;"),
        violation("R2", "use crate::infra::db::Pool;"),
        violation("R3", "use crate::infra::db::Pool;"),
        violation("R4", "use crate::infra::db::Pool;"),
        violation("R9", "use crate::infra::db::Pool;"),
        violation("R1", "use crate::infra::other::Thing;"),
        violation("R1", "use crate::domain::order::Order;"),
    ];
    let files = [change()];
    let brief = TicketBrief { context: "checkout".into(), ..TicketBrief::default() };
    let found = reviewer.review(&input(&files, &h.root), Some(&brief)).await.unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].rule_id, "R1");
    assert_eq!(found[0].rule_title, "Domain never imports infrastructure");
    assert!(found[0].approved);
    assert_eq!(found[0].file, "src/domain/order.rs");
    assert_eq!(found[0].suggestion, None);
    assert_eq!(found[1].rule_id, "R2");
    assert!(!found[1].approved);
    assert_eq!(*h.model.last_selected.lock().unwrap(), ["R1", "R2"]);
    assert_eq!(*h.model.last_rule_ids.lock().unwrap(), ["R1", "R2", "R4"], "rejected rules are never sent");
}

#[tokio::test]
async fn reviewer_skips_inferred_rules_when_disabled() {
    let (h, reviewer) = reviewer_with_rejected("review-approved-only", false).await;
    *h.model.violations.lock().unwrap() = vec![violation("R2", "use crate::infra::db::Pool;")];
    let files = [change()];
    let found = reviewer.review(&input(&files, &h.root), None).await.unwrap();
    assert!(found.is_empty());
    assert_eq!(*h.model.last_selected.lock().unwrap(), ["R1"]);
}

#[tokio::test]
async fn reviewer_without_reviewable_files_or_matching_rules_makes_no_call() {
    let h = standard("review-empty");
    let reviewer = RulebookReviewer::with_service(h.service.clone(), true, 7);
    assert!(reviewer.review(&input(&[], &h.root), None).await.unwrap().is_empty());
    assert_eq!(h.model.draft_calls(), 0);

    let only_web = harness(
        "review-no-match",
        FakeModel::drafting(vec![rule(
            "Web",
            RuleKind::Explicit,
            &["web/**"],
            vec![example("CLAUDE.md", 2, "must never import")],
        )]),
    );
    let reviewer = RulebookReviewer::with_service(only_web.service.clone(), true, 7);
    let files = [change()];
    assert!(reviewer.review(&input(&files, &only_web.root), None).await.unwrap().is_empty());
    assert_eq!(only_web.model.check_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reviewer_reports_build_failures_as_agent_errors() {
    let h = harness("review-fail", FakeModel { fail: true, ..FakeModel::default() });
    let reviewer = RulebookReviewer::with_service(h.service.clone(), true, 7);
    let files = [change()];
    let err = reviewer.review(&input(&files, &h.root), None).await.unwrap_err();
    assert!(err.0.contains("model down"));
}
