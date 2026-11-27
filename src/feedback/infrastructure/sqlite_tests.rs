use super::*;
use crate::feedback::application::{FeedbackService, FindingFacts};
use std::sync::Arc;

fn facts(category: &str, evidence: &str) -> FindingFacts {
    FindingFacts { file: "a.rs".into(), category: category.into(), source: "llm".into(), evidence: evidence.into() }
}

fn open(dir: &tempfile::TempDir) -> SqliteFeedbackStore {
    SqliteFeedbackStore::open(dir.path().join("data/qgate.db")).unwrap()
}

#[test]
fn persists_upserts_and_scopes_by_repo_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let svc = FeedbackService::new(Arc::new(open(&dir)));
    let f = facts("style", "x");
    let print = svc.record("a", &f, Verdict::Dismissed, Scope::Repo).unwrap();
    svc.record("a", &f, Verdict::Accepted, Scope::Repo).unwrap();
    svc.record("b", &f, Verdict::Dismissed, Scope::Repo).unwrap();
    drop(svc);

    let svc = FeedbackService::new(Arc::new(open(&dir)));
    let a = svc.judgments("a", std::slice::from_ref(&print)).unwrap();
    assert_eq!(a[0].verdict, Verdict::Accepted);
    let b = svc.judgments("b", std::slice::from_ref(&print)).unwrap();
    assert_eq!(b[0].verdict, Verdict::Dismissed);
    assert_eq!(svc.scores("a").unwrap()[0].judgments, 1);
    assert!(svc.remove("a", &print).unwrap());
    assert!(svc.judgments("a", &[print]).unwrap().is_empty());
}

#[test]
fn replays_in_recorded_order_and_honours_reset() {
    let dir = tempfile::tempdir().unwrap();
    let svc = FeedbackService::new(Arc::new(open(&dir)));
    for i in 0..10 {
        svc.record("r", &facts("style", &format!("e{i}")), Verdict::Dismissed, Scope::Repo).unwrap();
    }
    let before = svc.scores("r").unwrap();
    assert_eq!((before[0].score, before[0].active), (10, true));
    assert_eq!(
        svc.assess("r", &[facts("style", "n")]).unwrap()[0].cap,
        Some(crate::feedback::application::SeverityCap::Minor)
    );

    svc.reset("r", "llm/style").unwrap();
    svc.reset("r", "llm/style").unwrap();
    assert!(svc.scores("r").unwrap().is_empty());
    svc.record("r", &facts("style", "e0"), Verdict::Accepted, Scope::Repo).unwrap();
    let after = svc.scores("r").unwrap();
    assert_eq!((after[0].score, after[0].judgments, after[0].active), (100, 1, false));
}

#[test]
fn order_follows_seq_not_insertion_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    store.put("r", "f1", "llm/x", Verdict::Dismissed, Scope::Repo, chrono::Utc::now()).unwrap();
    store.put("r", "f2", "llm/x", Verdict::Accepted, Scope::Repo, chrono::Utc::now()).unwrap();
    store.put("r", "f1", "llm/x", Verdict::Accepted, Scope::Repo, chrono::Utc::now()).unwrap();
    let verdicts: Vec<_> = store.events("r").unwrap().into_iter().map(|e| e.verdict).collect();
    assert_eq!(verdicts, [Verdict::Accepted, Verdict::Accepted]);
    assert_eq!(current_version(&store.lock().unwrap()).unwrap(), MIGRATIONS.len());
}

#[test]
fn rejects_newer_schema_and_unknown_verdicts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("qgate.db");
    let store = SqliteFeedbackStore::open(&path).unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO feedback_judgments (repo, fingerprint, kind, verdict, seq, recorded_at)
             VALUES ('r', 'f', 'k', 'weird', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }
    assert!(matches!(store.find("r", &["f".into()]), Err(FeedbackError::Storage(_))));
    assert!(matches!(store.events("r"), Err(FeedbackError::Storage(_))));
    store.lock().unwrap().execute("INSERT INTO feedback_schema_version VALUES (99)", []).unwrap();
    drop(store);
    assert!(matches!(SqliteFeedbackStore::open(&path), Err(FeedbackError::Storage(_))));
}

#[test]
fn open_fails_when_parent_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, "x").unwrap();
    assert!(matches!(SqliteFeedbackStore::open(blocker.join("db.sqlite")), Err(FeedbackError::Storage(_))));
}

fn insert_raw(store: &SqliteFeedbackStore, fingerprint: &str, scope: &str, recorded_at: &str) {
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO feedback_judgments (repo, fingerprint, kind, verdict, seq, recorded_at, scope)
             VALUES ('r', ?1, 'k', 'dismissed', 1, ?2, ?3)",
            params![fingerprint, recorded_at, scope],
        )
        .unwrap();
}

#[test]
fn rejects_unknown_scopes_and_unparseable_timestamps() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    insert_raw(&store, "bad-scope", "team", "2026-01-01T00:00:00Z");
    insert_raw(&store, "bad-time", "repo", "yesterday");

    assert!(matches!(store.find("r", &["bad-scope".into()]), Err(FeedbackError::Storage(_))));
    assert!(matches!(store.find("r", &["bad-time".into()]), Err(FeedbackError::Storage(_))));
}

#[test]
fn global_judgments_are_found_from_other_repos_and_the_repos_own_row_wins() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    let at = chrono::Utc::now();
    store.put("a", "f", "llm/x", Verdict::Dismissed, Scope::Global, at).unwrap();
    store.put("c", "f", "llm/x", Verdict::Accepted, Scope::Global, at).unwrap();

    let from_b = store.find("b", &["f".into()]).unwrap();
    assert_eq!((from_b[0].verdict, from_b[0].scope, from_b[0].repo.as_str()), (Verdict::Accepted, Scope::Global, "c"));
    assert_eq!(from_b[0].recorded_at.timestamp(), at.timestamp());
    assert_eq!(store.find("a", &["f".into()]).unwrap()[0].verdict, Verdict::Dismissed);

    store.put("b", "f", "llm/x", Verdict::Dismissed, Scope::Repo, at).unwrap();
    assert_eq!(store.find("b", &["f".into()]).unwrap()[0].scope, Scope::Repo);
    assert!(store.events("b").unwrap().len() == 1);

    assert!(store.delete("b", "f").unwrap());
    assert!(store.find("a", &["f".into()]).unwrap().is_empty());
    assert!(store.find("b", &["f".into()]).unwrap().is_empty());
}

#[test]
fn a_database_from_before_scopes_upgrades_with_existing_judgments_kept_per_repo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("qgate.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE feedback_schema_version (version INTEGER NOT NULL PRIMARY KEY);
             INSERT INTO feedback_schema_version VALUES (1);",
        )
        .unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute(
            "INSERT INTO feedback_judgments VALUES ('r', 'f', 'llm/x', 'dismissed', 1, '2026-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
    }

    let store = SqliteFeedbackStore::open(&path).unwrap();

    let found = store.find("r", &["f".into()]).unwrap();
    assert_eq!((found[0].verdict, found[0].scope), (Verdict::Dismissed, Scope::Repo));
    assert!(store.find("other", &["f".into()]).unwrap().is_empty());
    assert_eq!(current_version(&store.lock().unwrap()).unwrap(), MIGRATIONS.len());
}

fn verification(id: &str, text: &str, verdict: CriterionVerdict, at: &str) -> CriterionVerification {
    CriterionVerification {
        repo: "r".into(),
        criterion_id: id.into(),
        text_hash: crate::feedback::domain::criterion::criterion_text_hash(text),
        criterion_text: text.into(),
        verdict,
        note: "n".into(),
        recorded_at: DateTime::parse_from_rfc3339(at).unwrap().with_timezone(&Utc),
    }
}

#[test]
fn criterion_verifications_upsert_list_find_and_delete_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    let first = verification("K/AC-1", "one", CriterionVerdict::VerifiedMet, "2026-01-01T00:00:00+00:00");
    let second = verification("K/AC-2", "two", CriterionVerdict::Clarified, "2026-01-02T00:00:00+00:00");
    store.put_criterion(&first).unwrap();
    store.put_criterion(&second).unwrap();
    let updated = CriterionVerification { verdict: CriterionVerdict::NotApplicable, ..first.clone() };
    store.put_criterion(&updated).unwrap();
    drop(store);

    let store = open(&dir);
    let listed = store.list_criteria("r").unwrap();
    assert_eq!(listed, vec![second.clone(), updated.clone()]);
    assert!(store.list_criteria("other").unwrap().is_empty());
    let keys = [
        ("K/AC-1".to_string(), first.text_hash.clone()),
        ("K/AC-1".to_string(), "stale".to_string()),
        ("K/AC-2".to_string(), second.text_hash.clone()),
    ];
    assert_eq!(store.find_criteria("r", &keys).unwrap(), vec![updated, second]);
    assert!(store.delete_criterion("r", "K/AC-1", &first.text_hash).unwrap());
    assert!(!store.delete_criterion("r", "K/AC-1", &first.text_hash).unwrap());
}

#[test]
fn an_unknown_stored_criterion_verdict_is_a_storage_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO criterion_verifications VALUES ('r', 'i', 'h', 't', 'weird', '', '2026-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
    assert!(matches!(store.list_criteria("r"), Err(FeedbackError::Storage(_))));
}

#[test]
fn a_database_from_before_criteria_upgrades_and_keeps_judgments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("qgate.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE feedback_schema_version (version INTEGER NOT NULL PRIMARY KEY);
             INSERT INTO feedback_schema_version VALUES (1), (2);",
        )
        .unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.execute(
            "INSERT INTO feedback_judgments VALUES ('r', 'f', 'llm/x', 'dismissed', 1, '2026-01-01T00:00:00+00:00', 'repo')",
            [],
        )
        .unwrap();
    }

    let store = SqliteFeedbackStore::open(&path).unwrap();

    assert_eq!(store.find("r", &["f".into()]).unwrap().len(), 1);
    store.put_criterion(&verification("i", "t", CriterionVerdict::VerifiedMet, "2026-01-01T00:00:00+00:00")).unwrap();
    assert_eq!(store.list_criteria("r").unwrap().len(), 1);
    assert_eq!(current_version(&store.lock().unwrap()).unwrap(), MIGRATIONS.len());
}
