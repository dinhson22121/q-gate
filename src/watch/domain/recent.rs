//! Bounded memory of pushes already handled, so re-pushing the same sha to the same branch (or a
//! ref returning to a known sha) is ignored.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Default number of remembered pushes.
pub const DEFAULT_RECENT_CAPACITY: usize = 500;

/// `(repo, branch, sha)`: the same sha on another branch is distinct.
type Key = (PathBuf, String, String);

/// FIFO-bounded set of handled `(repo, branch, sha)` pushes.
#[derive(Debug, Clone)]
pub struct RecentCommits {
    capacity: usize,
    order: VecDeque<Key>,
    members: HashSet<Key>,
}

impl RecentCommits {
    /// `capacity` is clamped to at least 1.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self { capacity, order: VecDeque::with_capacity(capacity), members: HashSet::new() }
    }

    /// Remembers the push; returns false when it was already known.
    pub fn insert(&mut self, repo: PathBuf, branch: String, sha: String) -> bool {
        let key = (repo, branch, sha);
        if self.members.contains(&key) {
            return false;
        }
        if self.order.len() == self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.members.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.members.insert(key);
        true
    }

    pub fn contains(&self, repo: &Path, branch: &str, sha: &str) -> bool {
        self.members.contains(&(repo.to_path_buf(), branch.to_owned(), sha.to_owned()))
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

impl Default for RecentCommits {
    fn default() -> Self {
        Self::new(DEFAULT_RECENT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn duplicate_is_rejected_but_other_branches_are_distinct() {
        let mut recent = RecentCommits::new(10);
        assert!(recent.insert(repo("/a"), "feature".into(), "1".into()));
        assert!(recent.contains(Path::new("/a"), "feature", "1"));
        assert!(!recent.contains(Path::new("/a"), "feature", "2"));
        assert!(!recent.insert(repo("/a"), "feature".into(), "1".into()), "same push is a duplicate");
        assert!(recent.insert(repo("/a"), "other".into(), "1".into()), "same sha on another branch is distinct");
        assert!(recent.insert(repo("/b"), "feature".into(), "1".into()), "same sha in another repo is distinct");
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn oldest_entry_is_evicted_at_capacity() {
        let mut recent = RecentCommits::new(2);
        recent.insert(repo("/a"), "f".into(), "1".into());
        recent.insert(repo("/a"), "f".into(), "2".into());
        recent.insert(repo("/a"), "f".into(), "3".into());
        assert_eq!(recent.len(), 2);
        assert!(recent.insert(repo("/a"), "f".into(), "1".into()), "evicted entry is new again");
        assert!(!recent.insert(repo("/a"), "f".into(), "3".into()));
    }

    #[test]
    fn zero_capacity_is_clamped() {
        let mut recent = RecentCommits::new(0);
        assert!(recent.insert(repo("/a"), "f".into(), "1".into()));
        assert!(!recent.is_empty());
        assert!(!recent.insert(repo("/a"), "f".into(), "1".into()));
    }
}
