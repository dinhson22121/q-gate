//! The work item a change implements.

use serde::{Deserialize, Serialize};

use super::ticket_key::TicketKey;

/// Issue type names (lowercase) that end the parent walk.
const STORY_TYPES: &[&str] = &["story", "user story"];
/// Issue type names (lowercase) that are never merged into a ticket's requirements.
const EPIC_TYPES: &[&str] = &["epic", "initiative"];

/// The parent of a ticket as reported by the tracker (`fields.parent` in Jira).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentRef {
    pub key: TicketKey,
    /// Issue type name of the parent (e.g. "Story", "Epic"), when known.
    pub issue_type: Option<String>,
}

impl ParentRef {
    pub fn is_epic(&self) -> bool {
        is_one_of(self.issue_type.as_deref(), EPIC_TYPES)
    }
}

fn is_one_of(issue_type: Option<&str>, names: &[&str]) -> bool {
    issue_type.is_some_and(|t| names.contains(&t.trim().to_lowercase().as_str()))
}

/// A ticket as fetched from the tracker, with its acceptance criteria already extracted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub key: TicketKey,
    pub title: String,
    /// Browser URL of the ticket, when known.
    pub url: Option<String>,
    /// Workflow status name (e.g. "In Progress"), when known.
    pub status: Option<String>,
    /// Acceptance criteria in ticket order; empty when the ticket has none that could be parsed.
    pub criteria: Vec<String>,
    /// Issue type name (e.g. "Sub-task", "Story"). `None` when the tracker did not say, which also
    /// means the hierarchy (parent) is unknown.
    #[serde(default)]
    pub issue_type: Option<String>,
    #[serde(default)]
    pub parent: Option<ParentRef>,
    /// Full description as plain text (untruncated).
    #[serde(default)]
    pub description: String,
    /// Figma links from the description and the ticket's remote links, in order of appearance.
    #[serde(default)]
    pub design_links: Vec<String>,
    /// Tracker's last-modified marker (Jira `fields.updated`), compared verbatim.
    #[serde(default)]
    pub updated: Option<String>,
}

impl Ticket {
    /// A ticket with only a key and title; the other fields are empty.
    pub fn new(key: TicketKey, title: impl Into<String>) -> Self {
        Self {
            key,
            title: title.into(),
            url: None,
            status: None,
            criteria: Vec::new(),
            issue_type: None,
            parent: None,
            description: String::new(),
            design_links: Vec::new(),
            updated: None,
        }
    }

    pub fn is_story(&self) -> bool {
        is_one_of(self.issue_type.as_deref(), STORY_TYPES)
    }

    pub fn is_epic(&self) -> bool {
        is_one_of(self.issue_type.as_deref(), EPIC_TYPES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(raw: &str) -> TicketKey {
        TicketKey::parse(raw).unwrap()
    }

    #[test]
    fn classifies_issue_types_case_insensitively() {
        let mut ticket = Ticket::new(key("AB-1"), "t");
        assert!(!ticket.is_story());
        ticket.issue_type = Some(" STORY ".into());
        assert!(ticket.is_story());
        ticket.issue_type = Some("Epic".into());
        assert!(ticket.is_epic() && !ticket.is_story());
        let parent = |t: Option<&str>| ParentRef { key: key("AB-2"), issue_type: t.map(str::to_string) };
        assert!(parent(Some("Epic")).is_epic());
        assert!(!parent(Some("Story")).is_epic());
        assert!(!parent(None).is_epic());
    }

    #[test]
    fn round_trips_through_json_and_tolerates_old_entries() {
        let mut ticket = Ticket::new(key("AB-1"), "Title");
        ticket.parent = Some(ParentRef { key: key("AB-2"), issue_type: Some("Story".into()) });
        ticket.design_links = vec!["https://www.figma.com/design/k/x".into()];
        let json = serde_json::to_string(&ticket).unwrap();
        assert_eq!(serde_json::from_str::<Ticket>(&json).unwrap(), ticket);

        let old = r#"{"key":"AB-1","title":"t","url":null,"status":null,"criteria":["a"]}"#;
        assert_eq!(serde_json::from_str::<Ticket>(old).unwrap().criteria, vec!["a"]);
        assert!(serde_json::from_str::<Ticket>(r#"{"key":"bad","title":"t"}"#).is_err());
    }
}
