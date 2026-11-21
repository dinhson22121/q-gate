use std::fmt;
use std::time::Duration;

pub const DEFAULT_MAX_COMMIT_AGE: Duration = Duration::from_secs(60 * 60);
const PUSH_SUBJECT: &str = "update by push";
const FORCED_UPDATE_SUBJECT: &str = "forced-update";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogHead {
    pub sha: String,
    pub logged_at: i64,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefMove {
    Push { forced: bool },
    NotAPush { action: String },
    TooOld { age_secs: i64 },
    NoReflog,
}

impl RefMove {
    pub fn is_push(&self) -> bool {
        matches!(self, Self::Push { .. })
    }
}

impl fmt::Display for RefMove {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Push { forced: false } => f.write_str("pushed"),
            Self::Push { forced: true } => f.write_str("force-pushed"),
            Self::NotAPush { action } => write!(f, "ref moved by `{action}`, not by a push"),
            Self::TooOld { age_secs } => write!(f, "push is {} min old", age_secs / 60),
            Self::NoReflog => f.write_str("no reflog entry for the ref"),
        }
    }
}

pub fn is_push_subject(subject: &str) -> bool {
    subject.trim() == PUSH_SUBJECT
}

pub fn is_forced_update_subject(subject: &str) -> bool {
    subject.trim() == FORCED_UPDATE_SUBJECT
}

pub fn classify_ref_move(entry: Option<&ReflogHead>, now: i64, max_age: Duration) -> RefMove {
    let Some(entry) = entry else { return RefMove::NoReflog };
    let forced = is_forced_update_subject(&entry.subject);
    if !forced && !is_push_subject(&entry.subject) {
        return RefMove::NotAPush { action: entry.subject.trim().to_owned() };
    }
    let age_secs = now - entry.logged_at;
    let max_age_secs = i64::try_from(max_age.as_secs()).unwrap_or(i64::MAX);
    if age_secs.abs() > max_age_secs {
        return RefMove::TooOld { age_secs };
    }
    RefMove::Push { forced }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const MAX_AGE: Duration = DEFAULT_MAX_COMMIT_AGE;

    fn entry(subject: &str, logged_at: i64) -> ReflogHead {
        ReflogHead { sha: "abc".into(), logged_at, subject: subject.into() }
    }

    fn classify(subject: &str, logged_at: i64) -> RefMove {
        classify_ref_move(Some(&entry(subject, logged_at)), NOW, MAX_AGE)
    }

    #[test]
    fn a_recent_push_is_reviewed() {
        assert_eq!(classify("update by push", NOW - 5), RefMove::Push { forced: false });
    }

    #[test]
    fn a_force_push_is_reviewed_and_flagged() {
        assert_eq!(classify("forced-update", NOW - 5), RefMove::Push { forced: true });
    }

    #[test]
    fn a_fetch_or_pull_is_not_a_push() {
        for subject in ["fetch", "fetch origin", "pull", "pull: Fast-forward", "branch: Created from HEAD", ""] {
            assert!(matches!(classify(subject, NOW), RefMove::NotAPush { .. }), "{subject}");
        }
    }

    #[test]
    fn an_old_push_is_too_old() {
        let age_secs = MAX_AGE.as_secs() as i64 + 1;
        assert_eq!(classify("update by push", NOW - age_secs), RefMove::TooOld { age_secs });
    }

    #[test]
    fn a_missing_reflog_is_reported() {
        assert_eq!(classify_ref_move(None, NOW, MAX_AGE), RefMove::NoReflog);
    }

    #[test]
    fn max_age_is_configurable() {
        let e = entry("update by push", NOW - 120);
        assert_eq!(classify_ref_move(Some(&e), NOW, Duration::from_secs(60)), RefMove::TooOld { age_secs: 120 });
        assert_eq!(classify_ref_move(Some(&e), NOW, Duration::from_secs(300)), RefMove::Push { forced: false });
    }

    #[test]
    fn small_future_skew_is_tolerated() {
        assert_eq!(classify("update by push", NOW + 30), RefMove::Push { forced: false });
    }

    #[test]
    fn huge_max_age_does_not_overflow() {
        assert_eq!(
            classify_ref_move(Some(&entry("update by push", 0)), NOW, Duration::MAX),
            RefMove::Push { forced: false }
        );
    }

    #[test]
    fn subject_helpers_trim_whitespace() {
        assert!(is_push_subject("  update by push  "));
        assert!(is_forced_update_subject("forced-update\n"));
        assert!(!is_push_subject("update by push (manual)"));
    }

    #[test]
    fn moves_render_readably() {
        assert_eq!(RefMove::Push { forced: false }.to_string(), "pushed");
        assert_eq!(RefMove::Push { forced: true }.to_string(), "force-pushed");
        assert_eq!(RefMove::TooOld { age_secs: 7200 }.to_string(), "push is 120 min old");
        assert_eq!(RefMove::NotAPush { action: "fetch".into() }.to_string(), "ref moved by `fetch`, not by a push");
        assert!(!RefMove::NoReflog.to_string().is_empty());
        assert!(RefMove::Push { forced: false }.is_push() && !RefMove::NoReflog.is_push());
    }
}
