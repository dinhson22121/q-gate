use std::sync::{Arc, Mutex};

use super::*;
use crate::feedback::application::SeverityCap;
use crate::feedback::domain::scoring::KindEvent;

#[derive(Default)]
struct State {
    seq: u64,
    judgments: HashMap<(String, String), (String, Verdict, u64)>,
    resets: HashMap<(String, String), u64>,
}

#[derive(Default)]
struct MemoryStore(Mutex<State>);

impl FeedbackStore for MemoryStore {
    fn put(&self, repo: &str, fingerprint: &str, kind: &str, verdict: Verdict) -> Result<(), FeedbackError> {
        let mut state = self.0.lock().unwrap();
        state.seq += 1;
        let seq = state.seq;
        state.judgments.insert((repo.into(), fingerprint.into()), (kind.into(), verdict, seq));
        Ok(())
    }

    fn delete(&self, repo: &str, fingerprint: &str) -> Result<bool, FeedbackError> {
        Ok(self.0.lock().unwrap().judgments.remove(&(repo.into(), fingerprint.into())).is_some())
    }

    fn find(&self, repo: &str, fingerprints: &[String]) -> Result<Vec<Judgment>, FeedbackError> {
        let state = self.0.lock().unwrap();
        Ok(fingerprints
            .iter()
            .filter_map(|f| {
                state
                    .judgments
                    .get(&(repo.to_string(), f.clone()))
                    .map(|(_, verdict, _)| Judgment { fingerprint: f.clone(), verdict: *verdict })
            })
            .collect())
    }

    fn events(&self, repo: &str) -> Result<Vec<KindEvent>, FeedbackError> {
        let state = self.0.lock().unwrap();
        let mut rows: Vec<_> = state
            .judgments
            .iter()
            .filter(|((r, _), (kind, _, seq))| {
                r == repo && *seq > state.resets.get(&(repo.to_string(), kind.clone())).copied().unwrap_or(0)
            })
            .map(|(_, (kind, verdict, seq))| (*seq, KindEvent { kind: kind.clone(), verdict: *verdict }))
            .collect();
        rows.sort_by_key(|(seq, _)| *seq);
        Ok(rows.into_iter().map(|(_, e)| e).collect())
    }

    fn reset(&self, repo: &str, kind: &str) -> Result<(), FeedbackError> {
        let mut state = self.0.lock().unwrap();
        state.seq += 1;
        let seq = state.seq;
        state.resets.insert((repo.into(), kind.into()), seq);
        Ok(())
    }
}

struct FailingStore;

impl FeedbackStore for FailingStore {
    fn put(&self, _: &str, _: &str, _: &str, _: Verdict) -> Result<(), FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }
    fn delete(&self, _: &str, _: &str) -> Result<bool, FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }
    fn find(&self, _: &str, _: &[String]) -> Result<Vec<Judgment>, FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }
    fn events(&self, _: &str) -> Result<Vec<KindEvent>, FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }
    fn reset(&self, _: &str, _: &str) -> Result<(), FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }
}

fn service() -> FeedbackService {
    FeedbackService::new(Arc::new(MemoryStore::default()))
}

fn facts(category: &str, evidence: &str) -> FindingFacts {
    FindingFacts { file: "a.rs".into(), category: category.into(), source: "llm".into(), evidence: evidence.into() }
}

fn dismiss_many(svc: &FeedbackService, category: &str, n: usize) {
    for i in 0..n {
        svc.record("r", &facts(category, &format!("e{i}")), Verdict::Dismissed).unwrap();
    }
}

#[test]
fn record_returns_fingerprint_and_upserts_verdict() {
    let svc = service();
    let f = facts("style", "x");
    let print = svc.record("r", &f, Verdict::Dismissed).unwrap();
    assert_eq!(print, fingerprint(&f));
    svc.record("r", &f, Verdict::Accepted).unwrap();
    let found = svc.judgments("r", std::slice::from_ref(&print)).unwrap();
    assert_eq!(found, vec![Judgment { fingerprint: print.clone(), verdict: Verdict::Accepted }]);
    assert_eq!(svc.scores("r").unwrap()[0].judgments, 1);
    assert!(svc.judgments("other", &[print]).unwrap().is_empty());
}

#[test]
fn remove_undoes_a_judgment() {
    let svc = service();
    let print = svc.record("r", &facts("style", "x"), Verdict::Dismissed).unwrap();
    assert!(svc.remove("r", &print).unwrap());
    assert!(!svc.remove("r", &print).unwrap());
    assert!(svc.scores("r").unwrap().is_empty());
}

#[test]
fn validates_inputs() {
    let svc = service();
    let f = facts("style", "x");
    assert!(matches!(svc.record(" ", &f, Verdict::Accepted), Err(FeedbackError::InvalidInput(_))));
    let no_source = FindingFacts { source: "".into(), ..f.clone() };
    assert!(matches!(svc.record("r", &no_source, Verdict::Accepted), Err(FeedbackError::InvalidInput(_))));
    let no_category = FindingFacts { category: "".into(), ..f };
    assert!(matches!(svc.record("r", &no_category, Verdict::Accepted), Err(FeedbackError::InvalidInput(_))));
    assert!(matches!(svc.remove("r", "ABC"), Err(FeedbackError::InvalidInput(_))));
    assert!(matches!(svc.remove("", &"a".repeat(64)), Err(FeedbackError::InvalidInput(_))));
    assert!(matches!(svc.assess("", &[]), Err(FeedbackError::InvalidInput(_))));
    assert!(matches!(svc.judgments("", &[]), Err(FeedbackError::InvalidInput(_))));
    assert!(matches!(svc.scores(""), Err(FeedbackError::InvalidInput(_))));
    assert!(matches!(svc.reset("r", ""), Err(FeedbackError::InvalidInput(_))));
    assert!(matches!(svc.reset("", "llm/style"), Err(FeedbackError::InvalidInput(_))));
}

#[test]
fn assess_keeps_order_and_reports_verdict_score_and_cap() {
    let svc = service();
    dismiss_many(&svc, "style", 10);
    let judged = facts("style", "e3");
    let fresh = facts("style", "never seen");
    let unknown_kind = facts("bug", "z");
    let result = svc.assess("r", &[fresh, judged, unknown_kind]).unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0], Assessment { verdict: None, cap: Some(SeverityCap::Minor), score: Some(10) });
    assert_eq!(result[1].verdict, Some(Verdict::Dismissed));
    assert_eq!(result[2], Assessment { verdict: None, cap: None, score: None });
}

#[test]
fn assess_reports_score_without_cap_while_inactive() {
    let svc = service();
    dismiss_many(&svc, "style", 3);
    let result = svc.assess("r", &[facts("style", "new")]).unwrap();
    assert_eq!(result[0], Assessment { verdict: None, cap: None, score: Some(70) });
}

#[test]
fn assess_never_caps_security() {
    let svc = service();
    dismiss_many(&svc, "security", 12);
    let result = svc.assess("r", &[facts("security", "new")]).unwrap();
    assert_eq!(result[0], Assessment { verdict: None, cap: None, score: Some(10) });
}

#[test]
fn assess_empty_input_returns_empty() {
    assert!(service().assess("r", &[]).unwrap().is_empty());
    assert!(service().judgments("r", &[]).unwrap().is_empty());
}

#[test]
fn reset_clears_counted_judgments_for_that_kind_only() {
    let svc = service();
    dismiss_many(&svc, "style", 10);
    dismiss_many(&svc, "bug", 2);
    svc.reset("r", "llm/style").unwrap();
    let scores = svc.scores("r").unwrap();
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0].kind, "llm/bug");
    svc.record("r", &facts("style", "after"), Verdict::Dismissed).unwrap();
    let style = svc.scores("r").unwrap().into_iter().find(|s| s.kind == "llm/style").unwrap();
    assert_eq!((style.score, style.judgments, style.active), (90, 1, false));
}

#[test]
fn changing_verdict_moves_judgment_to_latest_position() {
    let svc = service();
    let first = facts("style", "first");
    svc.record("r", &first, Verdict::Dismissed).unwrap();
    svc.record("r", &facts("style", "second"), Verdict::Accepted).unwrap();
    svc.record("r", &first, Verdict::Accepted).unwrap();
    let score = &svc.scores("r").unwrap()[0];
    assert_eq!((score.score, score.judgments), (100, 2));
}

#[test]
fn propagates_storage_errors() {
    let svc = FeedbackService::new(Arc::new(FailingStore));
    let f = facts("style", "x");
    assert!(matches!(svc.record("r", &f, Verdict::Accepted), Err(FeedbackError::Storage(_))));
    assert!(matches!(svc.remove("r", &"a".repeat(64)), Err(FeedbackError::Storage(_))));
    assert!(matches!(svc.assess("r", &[f]), Err(FeedbackError::Storage(_))));
    assert!(matches!(svc.judgments("r", &["x".into()]), Err(FeedbackError::Storage(_))));
    assert!(matches!(svc.scores("r"), Err(FeedbackError::Storage(_))));
    assert!(matches!(svc.reset("r", "k"), Err(FeedbackError::Storage(_))));
}

#[test]
fn verdict_serializes_snake_case_and_parses() {
    assert_eq!(serde_json::to_string(&Verdict::Accepted).unwrap(), "\"accepted\"");
    assert_eq!(Verdict::Dismissed.as_str(), "dismissed");
    assert_eq!(Verdict::parse("dismissed"), Some(Verdict::Dismissed));
    assert_eq!(Verdict::parse("accepted"), Some(Verdict::Accepted));
    assert_eq!(Verdict::parse("nope"), None);
    assert_eq!(serde_json::to_string(&SeverityCap::Minor).unwrap(), "\"minor\"");
}

#[test]
fn a_finding_the_user_accepted_is_never_capped() {
    let svc = service();
    dismiss_many(&svc, "performance", 10);
    let accepted = facts("performance", "kept");
    svc.record("r", &accepted, Verdict::Accepted).unwrap();
    let other = facts("performance", "fresh");

    let out = svc.assess("r", &[accepted, other]).unwrap();

    assert_eq!(out[0].cap, None);
    assert_eq!(out[1].cap, Some(SeverityCap::Minor));
}
