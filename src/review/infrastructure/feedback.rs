use std::sync::Arc;

use futures::future::BoxFuture;

use crate::feedback::application::{Assessment, FeedbackService, FindingFacts, SeverityCap, Verdict};
use crate::review::application::ports::FeedbackPolicy;
use crate::review::application::{FindingFeedback, finding_identity};
use crate::review::domain::finding::{Finding, Severity};

pub struct StoredFeedback {
    service: Arc<FeedbackService>,
    repo: String,
}

impl StoredFeedback {
    pub fn new(service: Arc<FeedbackService>, repo: String) -> Self {
        Self { service, repo }
    }
}

impl FeedbackPolicy for StoredFeedback {
    fn assess<'a>(&'a self, findings: &'a [Finding]) -> BoxFuture<'a, Vec<FindingFeedback>> {
        let facts: Vec<FindingFacts> = findings.iter().map(facts_of).collect();
        let (service, repo) = (self.service.clone(), self.repo.clone());
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || service.assess(&repo, &facts)).await {
                Ok(Ok(assessments)) => assessments.into_iter().map(to_feedback).collect(),
                Ok(Err(e)) => {
                    eprintln!("qgate: warning: feedback not applied: {e}");
                    Vec::new()
                }
                Err(e) => {
                    eprintln!("qgate: warning: feedback not applied: {e}");
                    Vec::new()
                }
            }
        })
    }
}

pub fn facts_of(finding: &Finding) -> FindingFacts {
    let id = finding_identity(finding);
    FindingFacts { file: id.file, category: id.category, source: id.source, evidence: id.evidence }
}

fn to_feedback(assessment: Assessment) -> FindingFeedback {
    FindingFeedback {
        dismissed: assessment.verdict == Some(Verdict::Dismissed),
        cap: assessment.cap.map(|cap| match cap {
            SeverityCap::Major => Severity::Major,
            SeverityCap::Minor => Severity::Minor,
        }),
        score: assessment.score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::application::Scope;
    use crate::feedback::infrastructure::SqliteFeedbackStore;
    use crate::review::domain::finding::fixtures::finding;

    fn service(dir: &tempfile::TempDir) -> Arc<FeedbackService> {
        let store = SqliteFeedbackStore::open(dir.path().join("qgate.db")).unwrap();
        Arc::new(FeedbackService::new(Arc::new(store)))
    }

    #[tokio::test]
    async fn dismissed_findings_and_low_precision_caps_come_back_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);
        let dismissed = finding("a.go", 1, Severity::Critical, "x := 1");
        service.record("repo", &facts_of(&dismissed), Verdict::Dismissed, Scope::Repo).unwrap();
        for i in 0..9 {
            let other = finding("b.go", i, Severity::Major, &format!("y{i}"));
            service.record("repo", &facts_of(&other), Verdict::Dismissed, Scope::Repo).unwrap();
        }
        let fresh = finding("c.go", 3, Severity::Critical, "z");
        let policy = StoredFeedback::new(service, "repo".into());

        let feedback = policy.assess(&[dismissed, fresh]).await;

        assert!(feedback[0].dismissed);
        assert!(!feedback[1].dismissed);
        assert_eq!(feedback[1].cap, Some(Severity::Minor));
        assert_eq!(feedback[1].score, Some(10));
    }

    #[tokio::test]
    async fn another_repository_is_not_affected() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);
        let f = finding("a.go", 1, Severity::Critical, "x := 1");
        service.record("repo", &facts_of(&f), Verdict::Dismissed, Scope::Repo).unwrap();

        let feedback = StoredFeedback::new(service, "other".into()).assess(&[f]).await;

        assert_eq!(feedback, vec![FindingFeedback::default()]);
    }
}
