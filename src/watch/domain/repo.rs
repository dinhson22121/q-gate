//! Value objects describing watched repositories, commit events and review verdicts.

use std::path::PathBuf;

/// Number of hex digits shown for an abbreviated commit id.
const SHORT_SHA_LEN: usize = 7;

/// A git repository whose commits are reviewed automatically.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WatchedRepo {
    /// Canonical path of the repository root (working tree top level).
    pub path: PathBuf,
    /// Human-readable name, the last path component by default.
    pub name: String,
}

impl WatchedRepo {
    /// Builds a repo named after the last component of `path`.
    pub fn from_path(path: PathBuf) -> Self {
        let name =
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        Self { path, name }
    }
}

/// A pushed branch of a watched repository: the range `base..sha` (the branch's own commits since
/// its base branch) is to be reviewed as one change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitEvent {
    pub repo: WatchedRepo,
    /// Pushed branch tip.
    pub sha: String,
    /// Merge base with the base branch; `None` when there is no merge base (orphan/unrelated
    /// history) and the whole branch is reviewed.
    pub base: Option<String>,
    /// The pushed branch name; `None` only in legacy fixtures.
    pub branch: Option<String>,
    /// Caveat to surface with the review (e.g. a missing reflog or merge base); usually `None`.
    pub note: Option<String>,
}

impl CommitEvent {
    pub fn short_sha(&self) -> &str {
        short_sha(&self.sha)
    }

    /// Replaces this queued event with a newer push of the same branch: the newer tip, base and
    /// note win (the base is the merge base with the base branch, recomputed for the new tip).
    pub fn coalesce(self, newer: CommitEvent) -> CommitEvent {
        newer
    }
}

/// Abbreviates a commit id (returns it unchanged when already short).
pub fn short_sha(sha: &str) -> &str {
    sha.get(..SHORT_SHA_LEN).unwrap_or(sha)
}

/// Outcome of reviewing one commit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommitVerdict {
    /// Whether the quality gate passed.
    pub passed: bool,
    /// Number of blocking findings (meaningful when `passed` is false).
    pub blocking_issues: usize,
    /// One-paragraph summary shown in the notification.
    pub summary: String,
    /// Id of the stored review run, used to link to the dashboard.
    pub run_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_defaults_to_last_path_component() {
        let repo = WatchedRepo::from_path(PathBuf::from("/repos/shop-api"));
        assert_eq!(repo.name, "shop-api");
    }

    #[test]
    fn name_falls_back_to_full_path_without_file_name() {
        let repo = WatchedRepo::from_path(PathBuf::from("/"));
        assert_eq!(repo.name, "/");
    }

    fn event(sha: &str, base: Option<&str>, branch: &str) -> CommitEvent {
        CommitEvent {
            repo: WatchedRepo::from_path(PathBuf::from("/repos/api")),
            sha: sha.into(),
            base: base.map(str::to_owned),
            branch: Some(branch.into()),
            note: None,
        }
    }

    #[test]
    fn coalesce_takes_the_newer_push() {
        let merged = event("c2", Some("b"), "feature").coalesce(event("c3", Some("b"), "feature"));
        assert_eq!(merged, event("c3", Some("b"), "feature"));
    }

    #[test]
    fn short_sha_abbreviates_long_ids_only() {
        assert_eq!(short_sha("0123456789abcdef"), "0123456");
        assert_eq!(short_sha("abc"), "abc");
    }
}
