use std::sync::Arc;

use futures::future::BoxFuture;

use crate::feedback::application::{CriterionVerdict, CriterionVerifications};
use crate::review::application::ports::HumanVerifications;
use crate::review::application::{AcceptanceCriterion, HumanInput, HumanMark};

pub struct StoredHumanVerifications {
    service: Arc<CriterionVerifications>,
    repo: String,
}

impl StoredHumanVerifications {
    pub fn new(service: Arc<CriterionVerifications>, repo: String) -> Self {
        Self { service, repo }
    }
}

impl HumanVerifications for StoredHumanVerifications {
    fn lookup<'a>(&'a self, criteria: &'a [AcceptanceCriterion]) -> BoxFuture<'a, Vec<HumanInput>> {
        let pairs: Vec<(String, String)> = criteria.iter().map(|c| (c.id.clone(), c.text.clone())).collect();
        let (service, repo) = (self.service.clone(), self.repo.clone());
        Box::pin(async move {
            let count = pairs.len();
            let defaults = || vec![HumanInput::default(); count];
            match tokio::task::spawn_blocking(move || service.lookup(&repo, &pairs)).await {
                Ok(Ok(found)) => found.into_iter().map(|v| v.map(to_input).unwrap_or_default()).collect(),
                Ok(Err(e)) => {
                    eprintln!("qgate: warning: human verifications not applied: {e}");
                    defaults()
                }
                Err(e) => {
                    eprintln!("qgate: warning: human verifications not applied: {e}");
                    defaults()
                }
            }
        })
    }
}

fn to_input(verification: crate::feedback::application::CriterionVerification) -> HumanInput {
    match verification.verdict {
        CriterionVerdict::VerifiedMet => HumanInput { mark: Some(HumanMark::VerifiedMet), clarification: None },
        CriterionVerdict::NotApplicable => HumanInput { mark: Some(HumanMark::NotApplicable), clarification: None },
        CriterionVerdict::Clarified => HumanInput { mark: None, clarification: Some(verification.note) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::infrastructure::SqliteFeedbackStore;

    fn service(dir: &tempfile::TempDir) -> Arc<CriterionVerifications> {
        let store = SqliteFeedbackStore::open(dir.path().join("qgate.db")).unwrap();
        Arc::new(CriterionVerifications::new(Arc::new(store)))
    }

    fn criteria() -> Vec<AcceptanceCriterion> {
        vec![
            AcceptanceCriterion::new("K/AC-1", "one"),
            AcceptanceCriterion::new("K/AC-2", "two"),
            AcceptanceCriterion::new("K/AC-3", "three"),
            AcceptanceCriterion::new("K/AC-4", "four"),
        ]
    }

    #[tokio::test]
    async fn maps_each_verdict_in_criterion_order() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);
        service.verify("repo", "K/AC-1", "one", CriterionVerdict::VerifiedMet, "").unwrap();
        service.verify("repo", "K/AC-3", "three", CriterionVerdict::Clarified, "CSV only").unwrap();
        service.verify("repo", "K/AC-4", "four", CriterionVerdict::NotApplicable, "").unwrap();
        let human = StoredHumanVerifications::new(service, "repo".into());

        let inputs = human.lookup(&criteria()).await;

        assert_eq!(
            inputs,
            vec![
                HumanInput { mark: Some(HumanMark::VerifiedMet), clarification: None },
                HumanInput::default(),
                HumanInput { mark: None, clarification: Some("CSV only".into()) },
                HumanInput { mark: Some(HumanMark::NotApplicable), clarification: None },
            ]
        );
    }

    #[tokio::test]
    async fn a_changed_text_or_another_repo_does_not_apply() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);
        service.verify("repo", "K/AC-1", "one", CriterionVerdict::VerifiedMet, "").unwrap();
        let edited = vec![AcceptanceCriterion::new("K/AC-1", "one, edited")];

        let same_repo = StoredHumanVerifications::new(service.clone(), "repo".into()).lookup(&edited).await;
        let other_repo = StoredHumanVerifications::new(service, "other".into()).lookup(&criteria()[..1]).await;

        assert_eq!(same_repo, vec![HumanInput::default()]);
        assert_eq!(other_repo, vec![HumanInput::default()]);
    }

    #[tokio::test]
    async fn storage_errors_yield_defaults_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(&dir);
        let human = StoredHumanVerifications::new(service, String::new());

        let inputs = human.lookup(&criteria()).await;

        assert_eq!(inputs, vec![HumanInput::default(); 4]);
    }
}
