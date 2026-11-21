use std::collections::HashMap;
use std::path::PathBuf;

pub type RefId = (PathBuf, String);

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
        assert_eq!(resolve_base_branch(&branches(&["feature"]), "release"), Some("release".into()));
    }
}
