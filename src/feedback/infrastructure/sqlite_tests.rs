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
    let print = svc.record("a", &f, Verdict::Dismissed).unwrap();
    svc.record("a", &f, Verdict::Accepted).unwrap();
    svc.record("b", &f, Verdict::Dismissed).unwrap();
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
        svc.record("r", &facts("style", &format!("e{i}")), Verdict::Dismissed).unwrap();
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
    svc.record("r", &facts("style", "e0"), Verdict::Accepted).unwrap();
    let after = svc.scores("r").unwrap();
    assert_eq!((after[0].score, after[0].judgments, after[0].active), (100, 1, false));
}

#[test]
fn order_follows_seq_not_insertion_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    store.put("r", "f1", "llm/x", Verdict::Dismissed).unwrap();
    store.put("r", "f2", "llm/x", Verdict::Accepted).unwrap();
    store.put("r", "f1", "llm/x", Verdict::Accepted).unwrap();
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
        conn.execute("INSERT INTO feedback_judgments VALUES ('r', 'f', 'k', 'weird', 1, 'now')", []).unwrap();
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
