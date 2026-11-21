use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};
use futures::future::BoxFuture;
use tempfile::TempDir;

use crate::review::infrastructure::local_git::{GIT_HARDENING_ARGS, GIT_HARDENING_ENV};
use crate::testing::application::ports::{ScenarioSnapshot, ScenarioSnapshots};

const GIT: &str = "git";
const SNAPSHOT_PREFIX: &str = "qgate-snapshot-";
const INDEX_DIR_PREFIX: &str = "qgate-snapshot-index-";
const CONFIG_NO_MATCH: i32 = 1;

pub struct Snapshot {
    dir: TempDir,
}

impl Snapshot {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

pub async fn resolve_commit(repo: &Path, rev: &str) -> Result<String> {
    ensure_safe_rev(rev)?;
    let output = tokio::process::Command::new(GIT)
        .args(GIT_HARDENING_ARGS)
        .args(["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])
        .envs(GIT_HARDENING_ENV.iter().copied())
        .current_dir(repo)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run git")?;
    if !output.status.success() {
        bail!("'{rev}' is not a commit in {}", repo.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn commit_messages(repo: &Path, base: Option<&str>, head: &str) -> Result<Vec<String>> {
    const MAX_COMMITS: &str = "200";
    const SEPARATOR: char = '\u{1e}';
    ensure_safe_rev(head)?;
    let range = match base {
        Some(base) => {
            ensure_safe_rev(base)?;
            format!("{base}..{head}")
        }
        None => head.to_string(),
    };
    let count = if base.is_some() { MAX_COMMITS } else { "1" };
    let mut args = vec!["log", "--no-color", "--format=%B%x1e", "--max-count", count];
    if base.is_some() {
        args.push("--first-parent");
    }
    let output = tokio::process::Command::new(GIT)
        .args(GIT_HARDENING_ARGS)
        .args(&args)
        .arg(&range)
        .arg("--")
        .envs(GIT_HARDENING_ENV.iter().copied())
        .current_dir(repo)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run git log")?;
    if !output.status.success() {
        bail!("git log failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split(SEPARATOR)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .collect())
}

pub async fn export_commit(repo: &Path, sha: &str, parent: &Path) -> Result<Snapshot> {
    if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid commit sha '{sha}'");
    }
    std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let dir = tempfile::Builder::new()
        .prefix(SNAPSHOT_PREFIX)
        .tempdir_in(parent)
        .with_context(|| format!("cannot create a snapshot directory in {}", parent.display()))?;
    let index_dir = tempfile::Builder::new()
        .prefix(INDEX_DIR_PREFIX)
        .tempdir_in(parent)
        .with_context(|| format!("cannot create an index directory in {}", parent.display()))?;
    let (repo, sha, target) = (repo.to_path_buf(), sha.to_string(), dir.path().to_path_buf());
    let index = index_dir.path().join("index");
    tokio::task::spawn_blocking(move || checkout_into(&repo, &sha, &index, &target)).await??;
    Ok(Snapshot { dir })
}

pub struct GitScenarioSnapshots {
    parent: PathBuf,
}

impl GitScenarioSnapshots {
    pub fn new(parent: PathBuf) -> Self {
        Self { parent }
    }
}

impl ScenarioSnapshots for GitScenarioSnapshots {
    fn create<'a>(&'a self, repo_root: &'a Path, commit: &'a str) -> BoxFuture<'a, Result<ScenarioSnapshot, String>> {
        Box::pin(async move {
            let snapshot = export_commit(repo_root, commit, &self.parent).await.map_err(|e| format!("{e:#}"))?;
            Ok(ScenarioSnapshot::new(snapshot.path().to_path_buf(), snapshot))
        })
    }
}

pub fn sweep_stale_snapshots(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        let is_snapshot = entry.file_name().to_str().is_some_and(|name| name.starts_with(SNAPSHOT_PREFIX));
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if !is_snapshot || !is_dir {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(entry.path()) {
            eprintln!("qgate: cannot remove stale snapshot {}: {e}", entry.path().display());
        }
    }
}

fn checkout_into(repo: &Path, sha: &str, index: &Path, target: &Path) -> Result<()> {
    let filter_overrides = neutralised_filters(repo)?;
    let git = |args: &[&str]| -> Result<Output> {
        Command::new(GIT)
            .args(GIT_HARDENING_ARGS)
            .args(["-c", "core.symlinks=true"])
            .args(&filter_overrides)
            .args(args)
            .envs(GIT_HARDENING_ENV.iter().copied())
            .env("GIT_INDEX_FILE", index)
            .current_dir(repo)
            .stdin(Stdio::null())
            .output()
            .context("failed to run git")
    };
    ensure_success("git read-tree", &git(&["read-tree", sha])?)?;
    let mut prefix = target.to_str().context("snapshot path is not valid UTF-8")?.to_string();
    if !prefix.ends_with('/') {
        prefix.push('/');
    }
    ensure_success("git checkout-index", &git(&["checkout-index", "--all", "--force", &format!("--prefix={prefix}")])?)
}

fn neutralised_filters(repo: &Path) -> Result<Vec<String>> {
    let output = Command::new(GIT)
        .args(GIT_HARDENING_ARGS)
        .args(["config", "--null", "--name-only", "--get-regexp", r"^filter\."])
        .envs(GIT_HARDENING_ENV.iter().copied())
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .context("failed to run git config")?;
    if output.status.code() == Some(CONFIG_NO_MATCH) {
        return Ok(Vec::new());
    }
    ensure_success("git config", &output)?;
    filter_overrides(&String::from_utf8_lossy(&output.stdout))
}

fn filter_overrides(keys: &str) -> Result<Vec<String>> {
    let mut drivers: Vec<&str> = keys
        .split('\0')
        .filter_map(|key| key.strip_prefix("filter.")?.rsplit_once('.').map(|(driver, _)| driver))
        .collect();
    drivers.sort_unstable();
    drivers.dedup();
    let mut overrides = Vec::new();
    for driver in drivers {
        if driver.contains('=') || driver.chars().any(char::is_control) {
            bail!("refusing to export: unsupported filter driver name {driver:?} in the repository config");
        }
        for setting in ["process=", "smudge=", "clean=", "required=false"] {
            overrides.extend(["-c".to_string(), format!("filter.{driver}.{setting}")]);
        }
    }
    Ok(overrides)
}

fn ensure_success(what: &str, output: &Output) -> Result<()> {
    if !output.status.success() {
        bail!("{what} failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn ensure_safe_rev(rev: &str) -> Result<()> {
    if rev.is_empty() || rev.starts_with('-') || rev.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("invalid revision '{rev}'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo_with(files: &[(&str, &str)]) -> TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        for (path, content) in files {
            let full = repo.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "first"]);
        repo
    }

    async fn export_head(repo: &Path, parent: &Path) -> Snapshot {
        let sha = resolve_commit(repo, "HEAD").await.unwrap();
        export_commit(repo, &sha, parent).await.unwrap()
    }

    #[tokio::test]
    async fn exports_committed_content_not_working_tree() {
        let repo = repo_with(&[("a.txt", "committed"), ("dir/b.txt", "nested")]);
        std::fs::write(repo.path().join("a.txt"), "dirty").unwrap();
        std::fs::write(repo.path().join("untracked.txt"), "x").unwrap();

        let parent = tempfile::tempdir().unwrap();
        let snapshot = export_head(repo.path(), parent.path()).await;
        assert_eq!(std::fs::read_to_string(snapshot.path().join("a.txt")).unwrap(), "committed");
        assert_eq!(std::fs::read_to_string(snapshot.path().join("dir/b.txt")).unwrap(), "nested");
        assert!(!snapshot.path().join("untracked.txt").exists());
        assert!(!snapshot.path().join("index").exists(), "the temporary index stays outside the snapshot");
        let leftovers: Vec<_> = std::fs::read_dir(parent.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "only the snapshot itself remains");

        let path = snapshot.path().to_path_buf();
        drop(snapshot);
        assert!(!path.exists(), "snapshot is removed on drop");
    }

    #[tokio::test]
    async fn scenario_snapshots_are_separate_fresh_exports() {
        let repo = repo_with(&[("a.txt", "committed")]);
        let sha = resolve_commit(repo.path(), "HEAD").await.unwrap();
        let parent = tempfile::tempdir().unwrap();
        let snapshots = GitScenarioSnapshots::new(parent.path().to_path_buf());

        let first = snapshots.create(repo.path(), &sha).await.unwrap();
        std::fs::write(first.path().join("a.txt"), "tampered").unwrap();
        let second = snapshots.create(repo.path(), &sha).await.unwrap();

        assert_ne!(first.path(), second.path());
        assert_eq!(std::fs::read_to_string(second.path().join("a.txt")).unwrap(), "committed");
        let path = first.path().to_path_buf();
        drop(first);
        assert!(!path.exists(), "removed when dropped");
        assert!(snapshots.create(repo.path(), "not-a-sha").await.unwrap_err().contains("invalid commit sha"));
    }

    #[tokio::test]
    async fn export_ignore_and_export_subst_are_not_honoured() {
        let attributes = "tests/** export-ignore\nversion.txt export-subst\n";
        let repo = repo_with(&[
            (".gitattributes", attributes),
            ("tests/failing_test.rs", "assert!(false);"),
            ("version.txt", "$Format:%H$"),
        ]);

        let parent = tempfile::tempdir().unwrap();
        let snapshot = export_head(repo.path(), parent.path()).await;
        assert!(snapshot.path().join("tests/failing_test.rs").exists(), "export-ignore'd file is checked out");
        assert_eq!(std::fs::read_to_string(snapshot.path().join("version.txt")).unwrap(), "$Format:%H$");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repository_filter_drivers_and_fsmonitor_are_not_run() {
        let marker_dir = tempfile::tempdir().unwrap();
        let marker = |name: &str| marker_dir.path().join(name);
        let repo = repo_with(&[(".gitattributes", "* filter=evil\n"), ("a.txt", "raw\n")]);
        let touch = |name: &str| format!("touch '{}'", marker(name).display());
        git(repo.path(), &["config", "filter.evil.smudge", &format!("{}; cat", touch("smudge"))]);
        git(repo.path(), &["config", "filter.evil.process", &touch("process")]);
        git(repo.path(), &["config", "filter.evil.required", "true"]);
        git(repo.path(), &["config", "core.fsmonitor", &touch("fsmonitor")]);

        let parent = tempfile::tempdir().unwrap();
        let snapshot = export_head(repo.path(), parent.path()).await;
        assert_eq!(std::fs::read_to_string(snapshot.path().join("a.txt")).unwrap(), "raw\n");
        for name in ["smudge", "process", "fsmonitor"] {
            assert!(!marker(name).exists(), "{name} command from the repository config was executed");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_are_recreated_as_links() {
        let repo = repo_with(&[("target.txt", "t")]);
        std::os::unix::fs::symlink("target.txt", repo.path().join("link")).unwrap();
        git(repo.path(), &["add", "link"]);
        git(repo.path(), &["commit", "-q", "-m", "link"]);

        let parent = tempfile::tempdir().unwrap();
        let snapshot = export_head(repo.path(), parent.path()).await;
        let link = snapshot.path().join("link");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(link).unwrap(), Path::new("target.txt"));
    }

    #[test]
    fn filter_overrides_cover_every_driver_once() {
        let overrides = filter_overrides("filter.lfs.smudge\0filter.lfs.process\0filter.a.b.clean\0").unwrap();
        assert_eq!(overrides.len(), 2 * 4 * 2);
        assert!(overrides.contains(&"filter.a.b.process=".to_string()));
        assert!(overrides.contains(&"filter.lfs.required=false".to_string()));
        assert!(filter_overrides("").unwrap().is_empty());
        assert!(filter_overrides("filter.x=y.smudge\0").is_err());
    }

    #[test]
    fn sweep_removes_only_prefixed_directories() {
        let parent = tempfile::tempdir().unwrap();
        let stale = parent.path().join("qgate-snapshot-abc123");
        std::fs::create_dir_all(stale.join("nested")).unwrap();
        std::fs::write(stale.join("nested/f"), "x").unwrap();
        let stale_index = parent.path().join("qgate-snapshot-index-xyz");
        std::fs::create_dir(&stale_index).unwrap();
        let other_dir = parent.path().join("keep-me");
        std::fs::create_dir(&other_dir).unwrap();
        let prefixed_file = parent.path().join("qgate-snapshot-file");
        std::fs::write(&prefixed_file, "x").unwrap();

        sweep_stale_snapshots(parent.path());

        assert!(!stale.exists() && !stale_index.exists());
        assert!(other_dir.exists() && prefixed_file.exists());
        sweep_stale_snapshots(&parent.path().join("missing"));
    }

    #[cfg(unix)]
    #[test]
    fn sweep_does_not_follow_symlinks() {
        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("precious"), "x").unwrap();
        let link = parent.path().join("qgate-snapshot-link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        sweep_stale_snapshots(parent.path());

        assert!(outside.path().join("precious").exists());
    }

    #[tokio::test]
    async fn rejects_unsafe_or_unknown_revisions() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        assert!(resolve_commit(repo.path(), "--output=/tmp/x").await.is_err());
        assert!(resolve_commit(repo.path(), "main branch").await.is_err());
        assert!(resolve_commit(repo.path(), "HEAD").await.is_err(), "no commits yet");
        assert!(export_commit(repo.path(), "-x", repo.path()).await.is_err());
    }

    #[tokio::test]
    async fn commit_messages_cover_the_whole_range() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        for message in ["first", "SHOP-1: second", "SHOP-2: third\n\nbody"] {
            std::fs::write(repo.path().join("f.txt"), message).unwrap();
            git(repo.path(), &["add", "."]);
            git(repo.path(), &["commit", "-q", "-m", message]);
        }
        let head = resolve_commit(repo.path(), "HEAD").await.unwrap();
        let base = resolve_commit(repo.path(), "HEAD~2").await.unwrap();
        let range = commit_messages(repo.path(), Some(&base), &head).await.unwrap();
        assert_eq!(range, vec!["SHOP-2: third\n\nbody", "SHOP-1: second"]);
        let single = commit_messages(repo.path(), None, &head).await.unwrap();
        assert_eq!(single, vec!["SHOP-2: third\n\nbody"]);
        assert!(commit_messages(repo.path(), Some("-x"), &head).await.is_err());
    }

    #[tokio::test]
    async fn range_messages_follow_first_parent_only() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("f.txt"), "base").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "base"]);
        let base = resolve_commit(repo.path(), "HEAD").await.unwrap();

        std::fs::write(repo.path().join("f.txt"), "main work").unwrap();
        git(repo.path(), &["commit", "-qam", "MAIN-99: unrelated"]);

        git(repo.path(), &["checkout", "-q", "-b", "feature", &base]);
        std::fs::write(repo.path().join("g.txt"), "feat").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "SHOP-1: feature work"]);
        git(repo.path(), &["merge", "-q", "--no-ff", "-m", "Merge main into feature", "main"]);
        let head = resolve_commit(repo.path(), "HEAD").await.unwrap();

        let messages = commit_messages(repo.path(), Some(&base), &head).await.unwrap();
        assert!(messages.iter().any(|m| m.contains("SHOP-1")), "feature commit is included");
        assert!(messages.iter().any(|m| m.contains("Merge main into feature")), "merge commit is included");
        assert!(!messages.iter().any(|m| m.contains("MAIN-99")), "merged-in base commit is excluded");
    }
}
