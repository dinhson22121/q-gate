use std::path::PathBuf;

const SHORT_SHA_LEN: usize = 7;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WatchedRepo {
    pub path: PathBuf,
    pub name: String,
}

impl WatchedRepo {
    pub fn from_path(path: PathBuf) -> Self {
        let name =
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        Self { path, name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitEvent {
    pub repo: WatchedRepo,
    pub sha: String,
    pub base: Option<String>,
    pub branch: Option<String>,
    pub note: Option<String>,
}

impl CommitEvent {
    pub fn short_sha(&self) -> &str {
        short_sha(&self.sha)
    }

    pub fn coalesce(self, newer: CommitEvent) -> CommitEvent {
        newer
    }
}

pub fn short_sha(sha: &str) -> &str {
    sha.get(..SHORT_SHA_LEN).unwrap_or(sha)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommitVerdict {
    pub passed: bool,
    pub blocking_issues: usize,
    pub summary: String,
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
