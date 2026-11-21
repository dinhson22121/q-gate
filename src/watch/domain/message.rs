//! Builds the notification shown when a commit review finishes.

use super::repo::{CommitEvent, CommitVerdict};

/// Text of a user notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationText {
    pub title: String,
    pub body: String,
}

/// Notification for a finished review: passed, failed with blocking issues, or errored.
pub fn review_notification(event: &CommitEvent, outcome: &Result<CommitVerdict, String>) -> NotificationText {
    let subject = format!("{} {}", event.repo.name, event.short_sha());
    match outcome {
        Ok(verdict) if verdict.passed => {
            NotificationText { title: format!("✅ qgate: {subject} passed"), body: verdict.summary.clone() }
        }
        Ok(verdict) => {
            let plural = if verdict.blocking_issues == 1 { "" } else { "s" };
            NotificationText {
                title: format!(
                    "❌ qgate: {subject} — {} blocking issue{plural} — open dashboard",
                    verdict.blocking_issues
                ),
                body: verdict.summary.clone(),
            }
        }
        Err(error) => NotificationText { title: format!("⚠️ qgate: {subject} review error"), body: error.clone() },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::watch::domain::repo::WatchedRepo;

    fn event() -> CommitEvent {
        CommitEvent {
            repo: WatchedRepo::from_path(PathBuf::from("/repos/api")),
            sha: "0123456789abcdef".into(),
            base: None,
            branch: Some("main".into()),
            note: None,
        }
    }

    #[test]
    fn passed_review() {
        let verdict = CommitVerdict { passed: true, summary: "all good".into(), ..Default::default() };
        let text = review_notification(&event(), &Ok(verdict));
        assert_eq!(text.title, "✅ qgate: api 0123456 passed");
        assert_eq!(text.body, "all good");
    }

    #[test]
    fn failed_review_counts_blocking_issues() {
        let verdict = CommitVerdict { passed: false, blocking_issues: 3, ..Default::default() };
        let text = review_notification(&event(), &Ok(verdict));
        assert_eq!(text.title, "❌ qgate: api 0123456 — 3 blocking issues — open dashboard");
    }

    #[test]
    fn single_blocking_issue_is_singular() {
        let verdict = CommitVerdict { passed: false, blocking_issues: 1, ..Default::default() };
        let text = review_notification(&event(), &Ok(verdict));
        assert!(text.title.contains("1 blocking issue —"), "{}", text.title);
    }

    #[test]
    fn review_error() {
        let text = review_notification(&event(), &Err("API down".into()));
        assert_eq!(text.title, "⚠️ qgate: api 0123456 review error");
        assert_eq!(text.body, "API down");
    }
}
