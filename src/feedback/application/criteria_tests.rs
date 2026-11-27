use std::sync::Mutex;

use chrono::{DateTime, TimeZone, Utc};

use super::*;

#[derive(Default)]
struct MemoryStore(Mutex<HashMap<(String, String, String), CriterionVerification>>);

impl CriterionStore for MemoryStore {
    fn put_criterion(&self, v: &CriterionVerification) -> Result<(), FeedbackError> {
        self.0.lock().unwrap().insert((v.repo.clone(), v.criterion_id.clone(), v.text_hash.clone()), v.clone());
        Ok(())
    }

    fn delete_criterion(&self, repo: &str, id: &str, hash: &str) -> Result<bool, FeedbackError> {
        Ok(self.0.lock().unwrap().remove(&(repo.into(), id.into(), hash.into())).is_some())
    }

    fn list_criteria(&self, repo: &str) -> Result<Vec<CriterionVerification>, FeedbackError> {
        Ok(self.0.lock().unwrap().values().filter(|v| v.repo == repo).cloned().collect())
    }

    fn find_criteria(
        &self,
        repo: &str,
        keys: &[(String, String)],
    ) -> Result<Vec<CriterionVerification>, FeedbackError> {
        let rows = self.0.lock().unwrap();
        Ok(keys.iter().filter_map(|(id, hash)| rows.get(&(repo.into(), id.clone(), hash.clone())).cloned()).collect())
    }
}

struct FailingStore;

impl CriterionStore for FailingStore {
    fn put_criterion(&self, _: &CriterionVerification) -> Result<(), FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }

    fn delete_criterion(&self, _: &str, _: &str, _: &str) -> Result<bool, FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }

    fn list_criteria(&self, _: &str) -> Result<Vec<CriterionVerification>, FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }

    fn find_criteria(&self, _: &str, _: &[(String, String)]) -> Result<Vec<CriterionVerification>, FeedbackError> {
        Err(FeedbackError::Storage("down".into()))
    }
}

fn fixed_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

fn service() -> CriterionVerifications {
    CriterionVerifications::new(Arc::new(MemoryStore::default())).with_clock(Arc::new(fixed_time))
}

fn invalid<T>(r: Result<T, FeedbackError>) -> bool {
    matches!(r, Err(FeedbackError::InvalidInput(_)))
}

fn storage<T>(r: Result<T, FeedbackError>) -> bool {
    matches!(r, Err(FeedbackError::Storage(_)))
}

fn pair(id: &str, text: &str) -> (String, String) {
    (id.to_string(), text.to_string())
}

#[test]
fn verify_stores_and_upserts() {
    let svc = service();
    let first = svc.verify("r", "K/AC-1", " Total  includes tax ", CriterionVerdict::NotApplicable, "").unwrap();
    assert_eq!(first.criterion_text, "Total  includes tax");
    assert_eq!(first.recorded_at, fixed_time());
    svc.verify("r", "K/AC-1", "Total includes tax", CriterionVerdict::VerifiedMet, "checked by hand").unwrap();
    let listed = svc.list("r").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].verdict, listed[0].note.as_str()), (CriterionVerdict::VerifiedMet, "checked by hand"));
    assert!(svc.list("other").unwrap().is_empty());
}

#[test]
fn revoke_deletes_once() {
    let svc = service();
    svc.verify("r", "K/AC-1", "text", CriterionVerdict::VerifiedMet, "").unwrap();
    assert!(!svc.revoke("r", "K/AC-1", "different text").unwrap());
    assert!(svc.revoke("r", "K/AC-1", "text").unwrap());
    assert!(!svc.revoke("r", "K/AC-1", "text").unwrap());
}

#[test]
fn lookup_keeps_order_and_ignores_changed_text() {
    let svc = service();
    svc.verify("r", "K/AC-2", "two", CriterionVerdict::Clarified, "note").unwrap();
    svc.verify("r", "K/AC-3", "three", CriterionVerdict::VerifiedMet, "").unwrap();
    let found =
        svc.lookup("r", &[pair("K/AC-1", "one"), pair("K/AC-2", "two"), pair("K/AC-3", "three edited")]).unwrap();
    assert_eq!(found.len(), 3);
    assert!(found[0].is_none() && found[2].is_none());
    assert_eq!(found[1].as_ref().unwrap().verdict, CriterionVerdict::Clarified);
    assert!(svc.lookup("r", &[]).unwrap().is_empty());
}

#[test]
fn validates_inputs() {
    let svc = service();
    assert!(invalid(svc.verify(" ", "i", "t", CriterionVerdict::VerifiedMet, "")));
    assert!(invalid(svc.verify("r", "", "t", CriterionVerdict::VerifiedMet, "")));
    assert!(invalid(svc.verify("r", "i", " ", CriterionVerdict::VerifiedMet, "")));
    assert!(invalid(svc.verify("r", "i", "t", CriterionVerdict::Clarified, " ")));
    assert!(invalid(svc.revoke("", "i", "t")));
    assert!(invalid(svc.revoke("r", "", "t")));
    assert!(invalid(svc.revoke("r", "i", "")));
    assert!(invalid(svc.list("")));
    assert!(invalid(svc.lookup("", &[pair("i", "t")])));
}

#[test]
fn storage_failures_surface() {
    let svc = CriterionVerifications::new(Arc::new(FailingStore));
    assert!(storage(svc.verify("r", "i", "t", CriterionVerdict::VerifiedMet, "")));
    assert!(storage(svc.revoke("r", "i", "t")));
    assert!(storage(svc.list("r")));
    assert!(storage(svc.lookup("r", &[pair("i", "t")])));
}
