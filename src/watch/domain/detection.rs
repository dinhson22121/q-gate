//! Detects pushed branches by comparing two snapshots of a repository's remote-tracking refs.

use std::collections::HashMap;
use std::path::PathBuf;

/// Identifies one watched branch: the repository root and the (short) branch name under the remote.
pub type RefId = (PathBuf, String);

/// A remote-tracking ref whose target changed, or that appeared, since `previous`: the new sha and
/// the sha it moved from (`None` for a newly observed ref). Sorted for deterministic handling.
pub fn detect_ref_moves(
    previous: &HashMap<RefId, String>,
    current: &HashMap<RefId, String>,
) -> Vec<(RefId, String, Option<String>)> {
    let mut moves: Vec<(RefId, String, Option<String>)> = current
        .iter()
        .filter_map(|(id, sha)| match previous.get(id) {
            Some(old) if old == sha => None,
            Some(old) => Some((id.clone(), sha.clone(), Some(old.clone()))),
            None => Some((id.clone(), sha.clone(), None)),
        })
        .collect();
    moves.sort();
    moves
}

/// The branch a pushed branch is compared against: the configured one when set, otherwise `main`
/// then `master` if either exists among `branches`. `None` when none applies (merge-base is then
/// skipped and the whole branch reviewed). A configured base is returned even if absent from
/// `branches`, so a push to it is still recognised and skipped.
pub fn resolve_base_branch(branches: &[String], configured: &str) -> Option<String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return Some(configured.to_owned());
    }
    for candidate in ["main", "master"] {
        if branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(repo: &str, branch: &str) -> RefId {
        (PathBuf::from(repo), branch.to_owned())
    }

    fn map(entries: &[(&str, &str, &str)]) -> HashMap<RefId, String> {
        entries.iter().map(|(repo, branch, sha)| (id(repo, branch), (*sha).to_owned())).collect()
    }

    #[test]
    fn moved_ref_yields_its_old_and_new_sha() {
        let moves = detect_ref_moves(&map(&[("/a", "feature", "1")]), &map(&[("/a", "feature", "2")]));
        assert_eq!(moves, vec![(id("/a", "feature"), "2".to_owned(), Some("1".to_owned()))]);
    }

    #[test]
    fn a_new_ref_has_no_old_sha() {
        let moves = detect_ref_moves(&map(&[]), &map(&[("/a", "feature", "2")]));
        assert_eq!(moves, vec![(id("/a", "feature"), "2".to_owned(), None)]);
    }

    #[test]
    fn unchanged_ref_yields_nothing() {
        assert!(detect_ref_moves(&map(&[("/a", "feature", "1")]), &map(&[("/a", "feature", "1")])).is_empty());
    }

    #[test]
    fn a_deleted_ref_is_ignored() {
        // Present in previous, absent from current: no event (detection only walks current).
        assert!(detect_ref_moves(&map(&[("/a", "feature", "1")]), &map(&[])).is_empty());
    }

    #[test]
    fn several_moved_refs_are_sorted() {
        let moves =
            detect_ref_moves(&map(&[("/a", "b", "1"), ("/a", "a", "1")]), &map(&[("/a", "b", "2"), ("/a", "a", "2")]));
        let branches: Vec<&str> = moves.iter().map(|((_, b), ..)| b.as_str()).collect();
        assert_eq!(branches, vec!["a", "b"]);
    }

    #[test]
    fn base_branch_prefers_configured_then_main_then_master() {
        let branches = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(resolve_base_branch(&branches(&["main", "master", "dev"]), "dev"), Some("dev".into()));
        assert_eq!(resolve_base_branch(&branches(&["main", "master"]), ""), Some("main".into()));
        assert_eq!(resolve_base_branch(&branches(&["master", "feature"]), ""), Some("master".into()));
        assert_eq!(resolve_base_branch(&branches(&["feature"]), ""), None);
        // A configured base is honoured even when it is not among the known refs.
        assert_eq!(resolve_base_branch(&branches(&["feature"]), "release"), Some("release".into()));
    }
}
