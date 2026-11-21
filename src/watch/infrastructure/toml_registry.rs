use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::git::repo_root;
use crate::watch::application::ports::{RepoRegistry, RepoSource, WatchError, WatchedRepo};

const APP_DIR: &str = "qgate";
const FILE_NAME: &str = "repos.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
struct RepoFile {
    #[serde(default, rename = "repo")]
    repos: Vec<RepoEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepoEntry {
    path: PathBuf,
    name: String,
}

impl From<RepoEntry> for WatchedRepo {
    fn from(entry: RepoEntry) -> Self {
        Self { path: entry.path, name: entry.name }
    }
}

#[derive(Debug, Clone)]
pub struct TomlRepoRegistry {
    file: PathBuf,
}

impl TomlRepoRegistry {
    pub fn new(file: PathBuf) -> Self {
        Self { file }
    }

    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join(APP_DIR).join(FILE_NAME))
    }

    pub fn at_default_location() -> Result<Self, WatchError> {
        Self::default_path()
            .map(Self::new)
            .ok_or_else(|| WatchError::Config("no configuration directory on this platform".into()))
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    async fn load(&self) -> Result<RepoFile, WatchError> {
        let text = match tokio::fs::read_to_string(&self.file).await {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(RepoFile::default()),
            Err(e) => return Err(e.into()),
        };
        toml::from_str(&text).map_err(|e| WatchError::Registry(format!("{}: {e}", self.file.display())))
    }

    async fn save(&self, file: &RepoFile) -> Result<(), WatchError> {
        let text = toml::to_string(file).map_err(|e| WatchError::Registry(e.to_string()))?;
        if let Some(dir) = self.file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let tmp = self.file.with_extension(format!("toml.tmp{}", std::process::id()));
        tokio::fs::write(&tmp, text).await?;
        if let Err(e) = tokio::fs::rename(&tmp, &self.file).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e.into());
        }
        Ok(())
    }
}

async fn validated_root(path: &Path) -> Result<PathBuf, WatchError> {
    let invalid = |why: &str| WatchError::InvalidRepo(format!("{}: {why}", path.display()));
    let canonical = tokio::fs::canonicalize(path).await.map_err(|e| invalid(&e.to_string()))?;
    if !tokio::fs::metadata(&canonical).await?.is_dir() {
        return Err(invalid("not a directory"));
    }
    match repo_root(&canonical).await? {
        Some(root) if root == canonical => Ok(canonical),
        Some(root) => Err(invalid(&format!("inside repository {}; add its root", root.display()))),
        None => Err(invalid("not inside a git working tree")),
    }
}

impl RepoSource for TomlRepoRegistry {
    async fn list(&self) -> Result<Vec<WatchedRepo>, WatchError> {
        Ok(self.load().await?.repos.into_iter().map(WatchedRepo::from).collect())
    }
}

impl RepoRegistry for TomlRepoRegistry {
    async fn add(&self, path: &Path) -> Result<WatchedRepo, WatchError> {
        let root = validated_root(path).await?;
        let mut file = self.load().await?;
        if let Some(existing) = file.repos.iter().find(|entry| entry.path == root) {
            return Ok(existing.clone().into());
        }
        let repo = WatchedRepo::from_path(root);
        file.repos.push(RepoEntry { path: repo.path.clone(), name: repo.name.clone() });
        self.save(&file).await?;
        Ok(repo)
    }

    async fn remove(&self, path: &Path) -> Result<bool, WatchError> {
        let canonical = tokio::fs::canonicalize(path).await.unwrap_or_else(|_| path.to_path_buf());
        let mut file = self.load().await?;
        let before = file.repos.len();
        file.repos.retain(|entry| entry.path != canonical && entry.path != path);
        if file.repos.len() == before {
            return Ok(false);
        }
        self.save(&file).await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::git::test_support::init_repo;
    use super::*;

    fn registry(dir: &Path) -> TomlRepoRegistry {
        TomlRepoRegistry::new(dir.join("config").join(FILE_NAME))
    }

    #[tokio::test]
    async fn missing_file_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(registry(dir.path()).list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn add_list_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("my-repo");
        std::fs::create_dir(&repo_dir).unwrap();
        init_repo(&repo_dir);
        let registry = registry(dir.path());

        let added = registry.add(&repo_dir).await.unwrap();
        let canonical = std::fs::canonicalize(&repo_dir).unwrap();
        assert_eq!(added, WatchedRepo { path: canonical.clone(), name: "my-repo".into() });
        assert_eq!(registry.add(&repo_dir).await.unwrap(), added, "add is idempotent");
        assert_eq!(registry.list().await.unwrap(), vec![added]);

        assert!(registry.remove(&repo_dir).await.unwrap());
        assert!(!registry.remove(&repo_dir).await.unwrap());
        assert!(registry.list().await.unwrap().is_empty());
        let leftovers: Vec<_> =
            std::fs::read_dir(registry.file().parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers, vec![FILE_NAME], "no temporary file left behind");
    }

    #[tokio::test]
    async fn remove_works_after_the_directory_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("gone");
        std::fs::create_dir(&repo_dir).unwrap();
        init_repo(&repo_dir);
        let registry = registry(dir.path());
        let added = registry.add(&repo_dir).await.unwrap();
        std::fs::remove_dir_all(&repo_dir).unwrap();

        assert!(registry.remove(&added.path).await.unwrap());
    }

    #[tokio::test]
    async fn rejects_non_repository() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let registry = registry(dir.path());

        let error = registry.add(&plain).await.unwrap_err();
        assert!(matches!(error, WatchError::InvalidRepo(_)), "{error}");
        let missing = registry.add(&dir.path().join("missing")).await.unwrap_err();
        assert!(matches!(missing, WatchError::InvalidRepo(_)), "{missing}");
        assert!(registry.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_subdirectory_of_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let sub = dir.path().join("src");
        std::fs::create_dir(&sub).unwrap();

        let error = registry(dir.path()).add(&sub).await.unwrap_err();
        assert!(error.to_string().contains("add its root"), "{error}");
    }

    #[tokio::test]
    async fn corrupt_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        std::fs::create_dir_all(registry.file().parent().unwrap()).unwrap();
        std::fs::write(registry.file(), "repo = 42 [").unwrap();

        assert!(matches!(registry.list().await, Err(WatchError::Registry(_))));
    }
}
