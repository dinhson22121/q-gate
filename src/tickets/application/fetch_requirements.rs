use crate::review::application::{AcceptanceCriterion, Requirements};
use crate::tickets::domain::{Ticket, TicketKey, extract_ticket_key};

use super::ports::{TicketError, TicketSource};

const CRITERION_ID_PREFIX: &str = "AC-";

pub struct FetchRequirements<S> {
    source: S,
}

impl<S: TicketSource> FetchRequirements<S> {
    pub fn new(source: S) -> Self {
        Self { source }
    }

    pub async fn execute(
        &self,
        branch: Option<&str>,
        commit_message: Option<&str>,
        allowed_projects: &[String],
    ) -> Result<Option<Requirements>, TicketError> {
        let Some(key) = extract_ticket_key(branch, commit_message, allowed_projects) else {
            return Ok(None);
        };
        self.execute_for_key(&key).await.map(Some)
    }

    pub async fn execute_for_key(&self, key: &TicketKey) -> Result<Requirements, TicketError> {
        let ticket = self.source.fetch(key).await?;
        Ok(to_requirements(&ticket))
    }
}

pub fn to_requirements(ticket: &Ticket) -> Requirements {
    let criteria = ticket
        .criteria
        .iter()
        .enumerate()
        .map(|(index, text)| AcceptanceCriterion::new(format!("{CRITERION_ID_PREFIX}{}", index + 1), text.clone()))
        .collect();
    Requirements { key: ticket.key.to_string(), title: ticket.title.clone(), url: ticket.url.clone(), criteria }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct FakeSource {
        result: Result<Vec<String>, TicketError>,
        requested: Mutex<Vec<String>>,
    }

    impl FakeSource {
        fn new(result: Result<Vec<String>, TicketError>) -> Self {
            Self { result, requested: Mutex::new(Vec::new()) }
        }
    }

    impl TicketSource for FakeSource {
        async fn fetch(&self, key: &TicketKey) -> Result<Ticket, TicketError> {
            self.requested.lock().unwrap().push(key.to_string());
            self.result.clone().map(|criteria| Ticket {
                url: Some(format!("https://jira.example/browse/{key}")),
                status: Some("In Progress".into()),
                criteria,
                ..Ticket::new(key.clone(), "Export")
            })
        }
    }

    #[tokio::test]
    async fn returns_none_without_key_and_does_not_fetch() {
        let use_case = FetchRequirements::new(FakeSource::new(Ok(vec![])));
        assert_eq!(use_case.execute(Some("main"), Some("fix stuff"), &[]).await, Ok(None));
        assert!(use_case.source.requested.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn maps_ticket_to_numbered_requirements() {
        let use_case = FetchRequirements::new(FakeSource::new(Ok(vec!["a".into(), "b".into()])));
        let requirements = use_case.execute(Some("feature/abc-7-x"), None, &[]).await.unwrap().unwrap();
        assert_eq!(requirements.key, "ABC-7");
        assert_eq!(requirements.title, "Export");
        assert_eq!(requirements.url.as_deref(), Some("https://jira.example/browse/ABC-7"));
        let ids: Vec<_> = requirements.criteria.iter().map(|c| (c.id.as_str(), c.text.as_str())).collect();
        assert_eq!(ids, vec![("AC-1", "a"), ("AC-2", "b")]);
    }

    #[tokio::test]
    async fn ticket_without_criteria_yields_empty_requirements() {
        let use_case = FetchRequirements::new(FakeSource::new(Ok(vec![])));
        let requirements = use_case.execute(None, Some("ABC-1: x"), &[]).await.unwrap().unwrap();
        assert!(requirements.criteria.is_empty());
    }

    #[tokio::test]
    async fn propagates_source_errors() {
        let use_case = FetchRequirements::new(FakeSource::new(Err(TicketError::NotFound("ABC-1".into()))));
        let err = use_case.execute(None, Some("ABC-1: x"), &[]).await.unwrap_err();
        assert_eq!(err, TicketError::NotFound("ABC-1".into()));
        assert_eq!(err.to_string(), "ticket ABC-1 not found");
    }
}
