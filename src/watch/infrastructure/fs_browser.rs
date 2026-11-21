use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::watch::application::manage_repos::{ManageError, RepoBrowser};
use crate::watch::domain::browse::{Crumb, DirEntry, Listing};

pub const REPOS_ROOT_ENV: &str = "QGATE_REPOS_ROOT";
pub const DEFAULT_REPOS_ROOT: &str = "/repos";
const MAX_ENTRIES: usize = 1000;

pub fn repos_root_from_env() -> PathBuf {
    std::env::var_os(REPOS_ROOT_ENV)
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_REPOS_ROOT), PathBuf::from)
}

#[derive(Debug, Clone)]
pub struct FsRepoBrowser {
    base: PathBuf,
}

impl FsRepoBrowser {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn from_env() -> Self {
        Self::new(repos_root_from_env())
    }

    fn resolve(&self, path: &str) -> Result<(PathBuf, PathBuf), ManageError> {
        let base = fs::canonicalize(&self.base)
            .map_err(|e| ManageError::NotFound(format!("repositories folder {}: {e}", self.base.display())))?;
        let requested = Path::new(path.trim());
        if path.contains('\0') || requested.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(ManageError::OutsideBase);
        }
        let candidate = if requested.is_absolute() { requested.to_path_buf() } else { base.join(requested) };
        let canonical = fs::canonicalize(&candidate).map_err(|e| match e.kind() {
            ErrorKind::NotFound => ManageError::NotFound(path.to_owned()),
            _ => ManageError::Io(e),
        })?;
        if !canonical.starts_with(&base) {
            return Err(ManageError::OutsideBase);
        }
        if !canonical.is_dir() {
            return Err(ManageError::NotADirectory(path.to_owned()));
        }
        Ok((base, canonical))
    }

    fn list_dir(base: PathBuf, dir: PathBuf) -> Result<Listing, ManageError> {
        let mut entries = Vec::new();
        for item in fs::read_dir(&dir).map_err(ManageError::Io)? {
            let item = item.map_err(ManageError::Io)?;
            let name = item.file_name().to_string_lossy().into_owned();
            let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir || name.starts_with('.') {
                continue;
            }
            let path = item.path();
            let is_git_root = path.join(".git").exists();
            entries.push(DirEntry { name, path, is_git_root });
        }
        entries.sort_by_key(|e| e.name.to_lowercase());
        let truncated = entries.len() > MAX_ENTRIES;
        entries.truncate(MAX_ENTRIES);
        let parent = (dir != base).then(|| dir.parent().map(Path::to_path_buf)).flatten();
        Ok(Listing {
            crumbs: crumbs(&base, &dir),
            is_git_root: dir.join(".git").exists(),
            base,
            path: dir,
            parent,
            entries,
            truncated,
        })
    }
}

fn crumbs(base: &Path, dir: &Path) -> Vec<Crumb> {
    let base_name = base.file_name().map_or_else(|| base.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mut trail = vec![Crumb { name: base_name, path: base.to_path_buf() }];
    let mut current = base.to_path_buf();
    for part in dir.strip_prefix(base).into_iter().flat_map(Path::components) {
        current.push(part);
        trail.push(Crumb { name: part.as_os_str().to_string_lossy().into_owned(), path: current.clone() });
    }
    trail
}

impl RepoBrowser for FsRepoBrowser {
    async fn browse(&self, path: &str) -> Result<Listing, ManageError> {
        let (base, dir) = self.resolve(path)?;
        tokio::task::spawn_blocking(move || Self::list_dir(base, dir))
            .await
            .map_err(|e| ManageError::Io(std::io::Error::other(e)))?
    }

    async fn resolve_repo_root(&self, path: &str) -> Result<PathBuf, ManageError> {
        let (_, dir) = self.resolve(path)?;
        if dir.join(".git").exists() { Ok(dir) } else { Err(ManageError::NotGitRoot(path.to_owned())) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, FsRepoBrowser) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("repos");
        for sub in ["alpha/.git", "beta/nested/.git", "Gamma", ".hidden"] {
            fs::create_dir_all(base.join(sub)).unwrap();
        }
        fs::write(base.join("file.txt"), "x").unwrap();
        (dir, FsRepoBrowser::new(base))
    }

    #[tokio::test]
    async fn lists_subdirectories_sorted_and_flags_git_roots() {
        let (_dir, browser) = fixture();
        let listing = browser.browse("").await.unwrap();
        let names: Vec<_> = listing.entries.iter().map(|e| (e.name.as_str(), e.is_git_root)).collect();
        assert_eq!(names, vec![("alpha", true), ("beta", false), ("Gamma", false)], "no files, no dot dirs");
        assert_eq!(listing.parent, None);
        assert_eq!(listing.crumbs.len(), 1);
        assert!(!listing.is_git_root && !listing.truncated);
    }

    #[tokio::test]
    async fn browses_nested_with_breadcrumbs_and_parent() {
        let (_dir, browser) = fixture();
        let listing = browser.browse("beta/nested").await.unwrap();
        assert_eq!(listing.crumbs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["repos", "beta", "nested"]);
        assert_eq!(listing.parent.as_deref(), Some(listing.base.join("beta").as_path()));
        assert!(listing.is_git_root);
        let abs = browser.browse(listing.base.join("beta").to_str().unwrap()).await.unwrap();
        assert_eq!(abs.entries[0].name, "nested");
    }

    #[tokio::test]
    async fn rejects_traversal_and_outside_paths() {
        let (dir, browser) = fixture();
        for bad in ["..", "alpha/../..", "../repos", "a\0b"] {
            assert!(matches!(browser.browse(bad).await, Err(ManageError::OutsideBase)), "{bad:?}");
        }
        let outside = dir.path().to_str().unwrap();
        assert!(matches!(browser.browse(outside).await, Err(ManageError::OutsideBase)));
        assert!(matches!(browser.browse("/etc").await, Err(ManageError::OutsideBase)));
        assert!(matches!(browser.resolve_repo_root("/etc").await, Err(ManageError::OutsideBase)));
        assert!(matches!(browser.browse("missing").await, Err(ManageError::NotFound(_))));
        assert!(matches!(browser.browse("file.txt").await, Err(ManageError::NotADirectory(_))));
    }

    #[tokio::test]
    async fn sibling_with_the_base_as_name_prefix_is_outside() {
        let (dir, browser) = fixture();
        fs::create_dir(dir.path().join("repos-evil")).unwrap();
        let evil = dir.path().join("repos-evil");
        assert!(matches!(browser.browse(evil.to_str().unwrap()).await, Err(ManageError::OutsideBase)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escape_is_refused_and_not_listed() {
        let (dir, browser) = fixture();
        let secret = dir.path().join("secret");
        fs::create_dir_all(secret.join(".git")).unwrap();
        std::os::unix::fs::symlink(&secret, dir.path().join("repos").join("link")).unwrap();

        assert!(matches!(browser.browse("link").await, Err(ManageError::OutsideBase)));
        assert!(matches!(browser.resolve_repo_root("link").await, Err(ManageError::OutsideBase)));
        let names: Vec<_> = browser.browse("").await.unwrap().entries.into_iter().map(|e| e.name).collect();
        assert!(!names.contains(&"link".to_string()));
    }

    #[tokio::test]
    async fn resolves_only_git_roots() {
        let (_dir, browser) = fixture();
        let root = browser.resolve_repo_root("alpha").await.unwrap();
        assert!(root.ends_with("alpha"));
        assert!(matches!(browser.resolve_repo_root("Gamma").await, Err(ManageError::NotGitRoot(_))));
    }

    #[tokio::test]
    async fn missing_base_is_reported() {
        let browser = FsRepoBrowser::new(PathBuf::from("/definitely/not/here"));
        assert!(matches!(browser.browse("").await, Err(ManageError::NotFound(_))));
    }

    #[test]
    fn base_comes_from_the_environment_or_defaults() {
        if std::env::var_os(REPOS_ROOT_ENV).is_none() {
            assert_eq!(repos_root_from_env(), PathBuf::from(DEFAULT_REPOS_ROOT));
        }
    }
}
