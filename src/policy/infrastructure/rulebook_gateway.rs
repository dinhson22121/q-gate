//! `RulebookGateway` on the conventions context's `RulebookService`.

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::conventions::application::{ConventionsError, RuleRef, RuleStatus, RulebookService, TeamRule};
use crate::policy::application::{PolicyError, RuleWrite, RulebookGateway};
use crate::policy::domain::{ExistingRule, RuleApproval};

pub struct RulebookUseCases {
    service: Arc<RulebookService>,
}

impl RulebookUseCases {
    pub fn new(service: Arc<RulebookService>) -> Self {
        Self { service }
    }
}

fn policy_error(err: ConventionsError) -> PolicyError {
    match err {
        ConventionsError::Conflict(message) => PolicyError::Conflict(message),
        ConventionsError::NotFound(message) => PolicyError::NotFound(message),
        other => PolicyError::Storage(other.to_string()),
    }
}

fn team_rule(write: RuleWrite) -> TeamRule {
    TeamRule {
        replaces: write.replaces.map(|(id, title)| RuleRef { id, title }),
        title: write.title,
        description: write.description,
        scope_globs: write.scope_globs,
        status: match write.status {
            RuleApproval::Approved => RuleStatus::Approved,
            RuleApproval::Proposed => RuleStatus::Proposed,
        },
    }
}

impl RulebookGateway for RulebookUseCases {
    fn rules<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<Vec<ExistingRule>, PolicyError>> {
        Box::pin(async move {
            let rulebook = self.service.load(repo).await.map_err(policy_error)?;
            Ok(rulebook
                .map(|book| {
                    book.rules
                        .into_iter()
                        .map(|rule| ExistingRule {
                            id: rule.id,
                            title: rule.title,
                            description: rule.description,
                            status: rule.status.as_str().to_string(),
                            scope_globs: rule.scope_globs,
                        })
                        .collect()
                })
                .unwrap_or_default())
        })
    }

    fn repos(&self) -> BoxFuture<'_, Result<Vec<String>, PolicyError>> {
        Box::pin(async move { self.service.repos().await.map_err(policy_error) })
    }

    fn write<'a>(&'a self, repo: &'a str, rules: Vec<RuleWrite>) -> BoxFuture<'a, Result<Vec<String>, PolicyError>> {
        Box::pin(async move {
            let titles: Vec<String> = rules.iter().map(|r| r.title.clone()).collect();
            self.service
                .add_team_rules(repo, rules.into_iter().map(team_rule).collect())
                .await
                .map_err(policy_error)?;
            Ok(titles)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conventions::application::fakes::{FakeModel, MemoryRepo, MemoryStore};

    fn write(title: &str, replaces: Option<(&str, &str)>, status: RuleApproval) -> RuleWrite {
        RuleWrite {
            replaces: replaces.map(|(id, t)| (id.to_string(), t.to_string())),
            title: title.into(),
            description: "d".into(),
            scope_globs: vec!["src/**".into()],
            status,
        }
    }

    #[tokio::test]
    async fn writes_team_rules_and_reads_them_back() {
        let service = RulebookService::new(
            Arc::new(FakeModel::default()),
            Arc::new(MemoryRepo::default()),
            Arc::new(MemoryStore::default()),
        );
        let gateway = RulebookUseCases::new(Arc::new(service));
        assert!(gateway.rules("/repos/policy-gw").await.unwrap().is_empty());
        let titles = gateway
            .write(
                "/repos/policy-gw",
                vec![write("A", None, RuleApproval::Approved), write("B", None, RuleApproval::Proposed)],
            )
            .await
            .unwrap();
        assert_eq!(titles, vec!["A", "B"]);
        let rules = gateway.rules("/repos/policy-gw").await.unwrap();
        assert_eq!(
            rules.iter().map(|r| (r.id.as_str(), r.status.as_str())).collect::<Vec<_>>(),
            [("R1", "approved"), ("R2", "proposed")]
        );
        assert_eq!(gateway.repos().await.unwrap(), vec!["/repos/policy-gw".to_string()]);
        let stale =
            gateway.write("/repos/policy-gw", vec![write("C", Some(("R1", "Not A")), RuleApproval::Approved)]).await;
        assert!(matches!(stale, Err(PolicyError::Conflict(_))));
        assert!(matches!(policy_error(ConventionsError::NotFound("x".into())), PolicyError::NotFound(_)));
        assert!(matches!(policy_error(ConventionsError::Model("x".into())), PolicyError::Storage(_)));
    }
}
