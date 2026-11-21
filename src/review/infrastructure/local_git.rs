//! `ChangeSource` backed by the local `git` executable.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::process::{Output, Stdio};

use futures::{StreamExt, TryStreamExt, stream};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::unified_diff::parse_unified_diff;
use crate::review::application::ports::{ChangeSource, SourceError};
use crate::review::domain::change_set::{ChangeSet, ChangeStatus, DiffLine, FileChange, Hunk};

/// Object id of the empty tree, used as the base when there is no parent commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const GIT: &str = "git";
/// Config overrides for every git call on a possibly untrusted repository (repos are mounted with
/// `safe.directory '*'`, so their `.git/config` must not get to run programs): no fsmonitor daemon or
/// hook, no hooks, and no transport (which also rules out `core.sshCommand`).
pub const GIT_HARDENING_ARGS: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "protocol.allow=never",
    "-c",
    "core.sshCommand=false",
];
/// Environment for every git call: never prompt, never take optional locks, never page.
/// `GIT_CONFIG_NOSYSTEM` is deliberately not set: the Docker image relies on the system `safe.directory`.
pub const GIT_HARDENING_ENV: &[(&str, &str)] =
    &[("GIT_TERMINAL_PROMPT", "0"), ("GIT_OPTIONAL_LOCKS", "0"), ("GIT_PAGER", "cat")];
/// Options placed before every subcommand so paths are reported verbatim.
const GLOBAL_ARGS: &[&str] = &["-c", "core.quotepath=off"];
/// Diff options that keep the output stable regardless of user configuration.
const DIFF_ARGS: &[&str] = &[
    "diff",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--find-renames",
    "-U3",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];
/// Same file selection as [`DIFF_ARGS`], as a NUL-separated path list used to cross-check the parser.
const NAME_ONLY_ARGS: &[&str] = &["diff", "--no-ext-diff", "--no-textconv", "--find-renames", "--name-only", "-z"];
/// Exit status git uses for "not found" answers to `--quiet` queries.
const GIT_QUIET_MISS: i32 = 1;
/// How many file contents are loaded concurrently.
const CONTENT_CONCURRENCY: usize = 8;
/// Largest working-tree file read into memory. A larger untracked file is reported as binary (no
/// content, no hunks); a larger tracked file keeps its diff hunks but gets no full content.
pub const MAX_WORKING_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// What to compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTarget {
    /// Uncommitted changes (staged, unstaged and untracked) against `HEAD`.
    WorkingTree,
    /// Working tree against the merge base of the given ref and `HEAD`.
    Base(String),
    /// The changes introduced by a single commit.
    Commit(String),
    /// The combined changes of `base..head` (e.g. several new commits at once); content is read from `head`.
    Range { base: String, head: String },
}

/// A git repository on the local disk.
#[derive(Debug, Clone)]
pub struct LocalGit {
    root: PathBuf,
    target: DiffTarget,
}

/// Where the full new version of a file is read from.
enum ContentOrigin<'a> {
    /// The working tree below this canonical root.
    WorkingTree(&'a Path),
    Commit(&'a str),
}

/// Result of reading a working-tree file.
enum WorkingFile {
    /// Missing, not a regular file (symlink, directory) or resolving outside the repository.
    Absent,
    /// Larger than [`MAX_WORKING_FILE_BYTES`].
    Oversized,
    Content(Vec<u8>),
}

/// Returns the top-level directory of the repository containing `cwd`.
pub async fn discover_root(cwd: &Path) -> Result<PathBuf, SourceError> {
    let stdout = git_stdout(cwd, &["rev-parse", "--show-toplevel"]).await?;
    Ok(PathBuf::from(stdout.trim()))
}

impl LocalGit {
    /// Creates a source for `root`. Rejects refs that could be mistaken for git options.
    pub fn new(root: PathBuf, target: DiffTarget) -> Result<Self, SourceError> {
        match &target {
            DiffTarget::WorkingTree => {}
            DiffTarget::Base(rev) | DiffTarget::Commit(rev) => validate_rev(rev)?,
            DiffTarget::Range { base, head } => {
                validate_rev(base)?;
                validate_rev(head)?;
            }
        }
        Ok(Self { root, target })
    }

    /// Full sha of `HEAD`, or `None` when the repository has no commits yet.
    pub async fn head_commit(&self) -> Result<Option<String>, SourceError> {
        self.quiet_query(&["rev-parse", "--verify", "--quiet", "HEAD"]).await
    }

    /// Short name of the checked-out branch, or `None` when `HEAD` is detached.
    pub async fn current_branch(&self) -> Result<Option<String>, SourceError> {
        self.quiet_query(&["symbolic-ref", "--quiet", "--short", "HEAD"]).await
    }

    /// Full message (subject and body) of the given commit.
    pub async fn commit_message(&self, sha: &str) -> Result<String, SourceError> {
        validate_rev(sha)?;
        let commit = self.resolve_commit(sha).await?;
        let stdout = self.git(&["show", "-s", "--format=%B", &commit, "--"]).await?;
        Ok(stdout.trim_end().to_string())
    }

    /// Full sha of the first parent of `sha`, or `None` for a root commit.
    pub async fn parent_of(&self, sha: &str) -> Result<Option<String>, SourceError> {
        validate_rev(sha)?;
        let commit = self.resolve_commit(sha).await?;
        let stdout = self.git(&["rev-list", "--parents", "-n", "1", &commit, "--"]).await?;
        Ok(stdout.split_whitespace().nth(1).map(str::to_string))
    }

    /// Best common ancestor of `a` and `b`, or `None` when they share no history.
    pub async fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>, SourceError> {
        validate_rev(a)?;
        validate_rev(b)?;
        let output = run_git(&self.root, &["merge-base", a, b]).await?;
        if output.status.success() {
            return Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()));
        }
        if output.status.code() == Some(GIT_QUIET_MISS) {
            return Ok(None);
        }
        Err(git_error(&output))
    }

    /// Whether `ancestor` is an ancestor of (or equal to) `descendant`.
    pub async fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, SourceError> {
        validate_rev(ancestor)?;
        validate_rev(descendant)?;
        let output = run_git(&self.root, &["merge-base", "--is-ancestor", ancestor, descendant]).await?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(GIT_QUIET_MISS) => Ok(false),
            _ => Err(git_error(&output)),
        }
    }

    /// Content-addressed object id of `path` at `rev` (a blob for a file, a tree for a directory),
    /// via `git rev-parse <rev>:<path>`. `None` when the path does not exist there.
    pub async fn object_id_at(&self, rev: &str, path: &str) -> Result<Option<String>, SourceError> {
        validate_rev(rev)?;
        validate_repo_path(path)?;
        self.quiet_query(&["rev-parse", "--verify", "--quiet", &format!("{rev}:{path}")]).await
    }

    /// Object id of the tree at `dir` in `rev` (the root tree for an empty `dir` or `.`). `None`
    /// when `dir` does not exist there or is not a directory.
    pub async fn tree_id_at(&self, rev: &str, dir: &str) -> Result<Option<String>, SourceError> {
        validate_rev(rev)?;
        if dir.is_empty() || dir == "." {
            return self.quiet_query(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{tree}}")]).await;
        }
        let Some(object) = self.object_id_at(rev, dir).await? else { return Ok(None) };
        if self.git(&["cat-file", "-t", &object]).await?.trim() == "tree" { Ok(Some(object)) } else { Ok(None) }
    }

    /// Text of `path` at revision `rev` (a commit or tree, e.g. the empty tree).
    ///
    /// `Ok(None)` when the path does not exist there, is not a file, or is not UTF-8 text. An unknown
    /// revision or a file above [`MAX_WORKING_FILE_BYTES`] is an error.
    pub async fn read_file_at(&self, rev: &str, path: &str) -> Result<Option<String>, SourceError> {
        validate_rev(rev)?;
        validate_repo_path(path)?;
        let tree_spec = format!("{rev}^{{tree}}");
        let Some(tree) = self.quiet_query(&["rev-parse", "--verify", "--quiet", &tree_spec]).await? else {
            return Err(SourceError::Git(format!("unknown revision {rev:?}")));
        };
        let blob_spec = format!("{tree}:{path}");
        let Some(object) = self.quiet_query(&["rev-parse", "--verify", "--quiet", &blob_spec]).await? else {
            return Ok(None);
        };
        if self.git(&["cat-file", "-t", &object]).await?.trim() != "blob" {
            return Ok(None);
        }
        let size: u64 = self
            .git(&["cat-file", "-s", &object])
            .await?
            .trim()
            .parse()
            .map_err(|e| SourceError::Git(format!("unexpected object size: {e}")))?;
        if size > MAX_WORKING_FILE_BYTES {
            return Err(SourceError::InvalidInput(format!(
                "{path} at {rev} is larger than {MAX_WORKING_FILE_BYTES} bytes"
            )));
        }
        let output = run_git(&self.root, &["cat-file", "blob", &object]).await?;
        if !output.status.success() {
            return Err(git_error(&output));
        }
        Ok(decode_text(output.stdout))
    }

    /// Runs a `--quiet` query whose miss is reported by exit status 1 without output.
    async fn quiet_query(&self, args: &[&str]) -> Result<Option<String>, SourceError> {
        let output = run_git(&self.root, args).await?;
        if output.status.success() {
            return Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()));
        }
        if output.status.code() == Some(GIT_QUIET_MISS) {
            return Ok(None);
        }
        Err(git_error(&output))
    }

    async fn git(&self, args: &[&str]) -> Result<String, SourceError> {
        git_stdout(&self.root, args).await
    }

    async fn resolve_commit(&self, rev: &str) -> Result<String, SourceError> {
        let spec = format!("{rev}^{{commit}}");
        Ok(self.git(&["rev-parse", "--verify", &spec]).await?.trim().to_string())
    }

    /// First parent of `commit`, or the empty tree for a root commit.
    async fn parent_or_empty_tree(&self, commit: &str) -> Result<String, SourceError> {
        Ok(self.parent_of(commit).await?.unwrap_or_else(|| EMPTY_TREE.to_string()))
    }

    /// Base to compare the working tree against for `WorkingTree` and `Base` targets.
    async fn working_tree_base(&self) -> Result<String, SourceError> {
        match &self.target {
            DiffTarget::Base(rev) => {
                let stdout = self.git(&["merge-base", rev, "HEAD"]).await?;
                Ok(stdout.trim().to_string())
            }
            _ => Ok(self.head_commit().await?.unwrap_or_else(|| EMPTY_TREE.to_string())),
        }
    }

    /// Parses the unified diff, keeping only entries whose path also appears in the NUL-separated
    /// name list, so an oddly named file can never desynchronise the parser into inventing a path.
    async fn diff(&self, revs: &[&str]) -> Result<Vec<FileChange>, SourceError> {
        let with_revs = |base: &[&'static str]| {
            let mut args: Vec<&str> = base.to_vec();
            args.extend_from_slice(revs);
            args.push("--");
            args
        };
        let (patch_args, name_args) = (with_revs(DIFF_ARGS), with_revs(NAME_ONLY_ARGS));
        let (patch, names) = tokio::try_join!(self.git(&patch_args), self.git(&name_args))?;
        let files = parse_unified_diff(&patch).map_err(|e| SourceError::Git(format!("cannot parse diff: {e}")))?;
        Ok(retain_listed(files, &names))
    }

    async fn load_working_tree(&self) -> Result<Vec<FileChange>, SourceError> {
        let root = tokio::fs::canonicalize(&self.root).await?;
        let base = self.working_tree_base().await?;
        let tracked = self.diff(&[&base]).await?;
        let tracked = self.with_contents(tracked, ContentOrigin::WorkingTree(&root)).await?;
        let untracked = self.untracked_files(&root).await?;
        Ok(merge_untracked(tracked, untracked))
    }

    async fn load_commit(&self, rev: &str) -> Result<Vec<FileChange>, SourceError> {
        let commit = self.resolve_commit(rev).await?;
        let parent = self.parent_or_empty_tree(&commit).await?;
        let files = self.diff(&[&parent, &commit]).await?;
        self.with_contents(files, ContentOrigin::Commit(&commit)).await
    }

    async fn load_range(&self, base: &str, head: &str) -> Result<Vec<FileChange>, SourceError> {
        let base = self.resolve_commit(base).await?;
        let head = self.resolve_commit(head).await?;
        let files = self.diff(&[&base, &head]).await?;
        self.with_contents(files, ContentOrigin::Commit(&head)).await
    }

    async fn untracked_files(&self, root: &Path) -> Result<Vec<FileChange>, SourceError> {
        let stdout = self.git(&["ls-files", "--others", "--exclude-standard", "-z"]).await?;
        let paths: Vec<&str> = stdout.split('\0').filter(|p| !p.is_empty()).collect();
        let pending: Vec<_> = paths.into_iter().map(|path| self.untracked_file(root, path)).collect();
        let loaded: Vec<Option<FileChange>> = run_bounded(pending).await?;
        Ok(loaded.into_iter().flatten().collect())
    }

    /// Builds an all-added change for an untracked file; `None` for symlinks and vanished files.
    async fn untracked_file(&self, root: &Path, path: &str) -> Result<Option<FileChange>, SourceError> {
        let content = match self.read_working_file(root, path).await? {
            WorkingFile::Absent => return Ok(None),
            WorkingFile::Oversized => None,
            WorkingFile::Content(bytes) => decode_text(bytes),
        };
        let hunks = content.as_deref().map(all_added_hunk).into_iter().flatten().collect();
        Ok(Some(FileChange {
            path: path.to_string(),
            status: ChangeStatus::Added,
            is_binary: content.is_none(),
            hunks,
            new_content: content,
        }))
    }

    async fn with_contents(
        &self,
        files: Vec<FileChange>,
        origin: ContentOrigin<'_>,
    ) -> Result<Vec<FileChange>, SourceError> {
        let pending: Vec<_> = files.into_iter().map(|file| self.with_content(file, &origin)).collect();
        run_bounded(pending).await
    }

    async fn with_content(&self, file: FileChange, origin: &ContentOrigin<'_>) -> Result<FileChange, SourceError> {
        if file.is_binary || file.status == ChangeStatus::Deleted {
            return Ok(file);
        }
        let bytes = match origin {
            ContentOrigin::WorkingTree(root) => match self.read_working_file(root, &file.path).await? {
                WorkingFile::Content(bytes) => bytes,
                WorkingFile::Absent | WorkingFile::Oversized => return Ok(file),
            },
            ContentOrigin::Commit(commit) => self.read_committed_file(commit, &file.path).await?,
        };
        match decode_text(bytes) {
            Some(text) => Ok(FileChange { new_content: Some(text), ..file }),
            // Not text after all (e.g. Latin-1): the lossily decoded hunks would only mislead.
            None => Ok(FileChange { is_binary: true, hunks: Vec::new(), ..file }),
        }
    }

    /// Reads a regular file that resolves below the canonical `root`, at most [`MAX_WORKING_FILE_BYTES`].
    async fn read_working_file(&self, root: &Path, path: &str) -> Result<WorkingFile, SourceError> {
        let full = self.root.join(path);
        let metadata = match tokio::fs::symlink_metadata(&full).await {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WorkingFile::Absent),
            Err(e) => return Err(e.into()),
        };
        if !metadata.file_type().is_file() {
            return Ok(WorkingFile::Absent);
        }
        if metadata.len() > MAX_WORKING_FILE_BYTES {
            return Ok(WorkingFile::Oversized);
        }
        // A symlinked parent directory could point anywhere: only read what really lives in the repo.
        let canonical = match tokio::fs::canonicalize(&full).await {
            Ok(canonical) => canonical,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WorkingFile::Absent),
            Err(e) => return Err(e.into()),
        };
        if !canonical.starts_with(root) {
            return Ok(WorkingFile::Absent);
        }
        let file = tokio::fs::File::open(&canonical).await?;
        let mut bytes = Vec::new();
        // The file may grow between the size check and the read: never read more than the cap + 1.
        file.take(MAX_WORKING_FILE_BYTES + 1).read_to_end(&mut bytes).await?;
        if bytes.len() as u64 > MAX_WORKING_FILE_BYTES {
            return Ok(WorkingFile::Oversized);
        }
        Ok(WorkingFile::Content(bytes))
    }

    async fn read_committed_file(&self, commit: &str, path: &str) -> Result<Vec<u8>, SourceError> {
        let output = run_git(&self.root, &["show", "--no-textconv", &format!("{commit}:{path}")]).await?;
        if !output.status.success() {
            return Err(git_error(&output));
        }
        Ok(output.stdout)
    }
}

impl ChangeSource for LocalGit {
    async fn load(&self) -> Result<ChangeSet, SourceError> {
        let files = match &self.target {
            DiffTarget::WorkingTree | DiffTarget::Base(_) => self.load_working_tree().await?,
            DiffTarget::Commit(rev) => self.load_commit(rev).await?,
            DiffTarget::Range { base, head } => self.load_range(base, head).await?,
        };
        Ok(ChangeSet { files })
    }
}

/// Rejects empty refs, refs that git would parse as options, and refs with whitespace or control characters.
fn validate_rev(rev: &str) -> Result<(), SourceError> {
    let reason = if rev.is_empty() {
        "ref must not be empty"
    } else if rev.starts_with('-') {
        "ref must not start with '-'"
    } else if rev.chars().any(|c| c.is_whitespace() || c.is_control()) {
        "ref must not contain whitespace or control characters"
    } else {
        return Ok(());
    };
    Err(SourceError::InvalidInput(format!("{reason}: {rev:?}")))
}

/// Accepts only a relative, normalised path inside the repository (no `..`, no leading '-').
fn validate_repo_path(path: &str) -> Result<(), SourceError> {
    let is_plain = |c: Component<'_>| matches!(c, Component::Normal(_));
    let reason = if path.is_empty() {
        "path must not be empty"
    } else if path.starts_with('-') {
        "path must not start with '-'"
    } else if path.chars().any(char::is_control) {
        "path must not contain control characters"
    } else if path.starts_with(['/', '\\']) || !Path::new(path).components().all(is_plain) {
        "path must be relative and must not contain '.' or '..' components"
    } else {
        return Ok(());
    };
    Err(SourceError::InvalidInput(format!("{reason}: {path:?}")))
}

/// Drops parsed entries whose path is missing from the NUL-separated `names` list.
fn retain_listed(files: Vec<FileChange>, names: &str) -> Vec<FileChange> {
    let listed: HashSet<&str> = names.split('\0').filter(|n| !n.is_empty()).collect();
    files.into_iter().filter(|f| listed.contains(f.path.as_str())).collect()
}

/// Text content, or `None` when the bytes contain NUL or are not valid UTF-8.
fn decode_text(bytes: Vec<u8>) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// One hunk adding every line of `content`; `None` for an empty file.
fn all_added_hunk(content: &str) -> Option<Hunk> {
    let lines: Vec<DiffLine> = (1u32..)
        .zip(content.lines())
        .map(|(new_line, text)| DiffLine::Added { new_line, text: text.to_string() })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(Hunk { header: format!("@@ -0,0 +1,{} @@", lines.len()), new_start: 1, lines })
}

/// Appends untracked files; one that shadows a tracked entry (e.g. after `git rm --cached`) replaces it.
fn merge_untracked(tracked: Vec<FileChange>, untracked: Vec<FileChange>) -> Vec<FileChange> {
    let mut merged: Vec<FileChange> =
        tracked.into_iter().filter(|t| !untracked.iter().any(|u| u.path == t.path)).collect();
    merged.extend(untracked);
    merged
}

/// Awaits the futures in order, at most `CONTENT_CONCURRENCY` at a time, stopping at the first error.
async fn run_bounded<T, F>(pending: Vec<F>) -> Result<Vec<T>, SourceError>
where
    F: Future<Output = Result<T, SourceError>>,
{
    stream::iter(pending).buffered(CONTENT_CONCURRENCY).try_collect().await
}

async fn run_git(cwd: &Path, args: &[&str]) -> Result<Output, SourceError> {
    let output = Command::new(GIT)
        .args(GIT_HARDENING_ARGS)
        .args(GLOBAL_ARGS)
        .args(args)
        .envs(GIT_HARDENING_ENV.iter().copied())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    Ok(output)
}

async fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String, SourceError> {
    let output = run_git(cwd, args).await?;
    if !output.status.success() {
        return Err(git_error(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_error(output: &Output) -> SourceError {
    SourceError::Git(String::from_utf8_lossy(&output.stderr).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn sh_git(dir: &Path, args: &[&str]) -> String {
        let output = StdCommand::new(GIT)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        sh_git(dir.path(), &["init", "-q", "-b", "main"]);
        dir
    }

    fn write(dir: &TempDir, path: &str, content: &[u8]) {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::write(full, content).expect("write file");
    }

    fn commit_all(dir: &TempDir, message: &str) -> String {
        sh_git(dir.path(), &["add", "-A"]);
        sh_git(dir.path(), &["commit", "-q", "-m", message]);
        sh_git(dir.path(), &["rev-parse", "HEAD"])
    }

    fn source(dir: &TempDir, target: DiffTarget) -> LocalGit {
        LocalGit::new(dir.path().to_path_buf(), target).expect("valid target")
    }

    async fn load(dir: &TempDir, target: DiffTarget) -> ChangeSet {
        source(dir, target).load().await.expect("load succeeds")
    }

    #[tokio::test]
    async fn working_tree_includes_modified_staged_and_untracked_files() {
        let dir = init_repo();
        write(&dir, "a.rs", b"one\ntwo\n");
        write(&dir, "b.rs", b"keep\n");
        commit_all(&dir, "init");
        write(&dir, "a.rs", b"one\nTWO\n");
        write(&dir, "b.rs", b"keep\nstaged\n");
        sh_git(dir.path(), &["add", "b.rs"]);
        write(&dir, "new/c.py", b"x = 1\ny = 2\n");

        let set = load(&dir, DiffTarget::WorkingTree).await;

        let a = set.find("a.rs").expect("a.rs changed");
        assert_eq!(a.status, ChangeStatus::Modified);
        assert_eq!(a.new_content.as_deref(), Some("one\nTWO\n"));
        assert!(set.find("b.rs").is_some());
        let c = set.find("new/c.py").expect("untracked file listed");
        assert_eq!(c.status, ChangeStatus::Added);
        assert_eq!(c.changed_positions(), vec![1, 2]);
        assert_eq!(c.hunks[0].header, "@@ -0,0 +1,2 @@");
        assert_eq!(set.files.len(), 3);
    }

    #[tokio::test]
    async fn fresh_repository_without_commits_diffs_against_empty_tree() {
        let dir = init_repo();
        write(&dir, "staged.go", b"package main\n");
        sh_git(dir.path(), &["add", "staged.go"]);
        write(&dir, "loose.go", b"package loose\n");
        write(&dir, "empty.txt", b"");

        let git = source(&dir, DiffTarget::WorkingTree);
        let set = git.load().await.expect("load succeeds");

        assert_eq!(set.find("staged.go").expect("staged").status, ChangeStatus::Added);
        assert_eq!(set.find("loose.go").expect("untracked").status, ChangeStatus::Added);
        assert!(!set.find("empty.txt").expect("empty file").has_content_change());
        assert_eq!(git.head_commit().await.expect("query"), None);
        assert_eq!(git.current_branch().await.expect("query"), Some("main".into()));
    }

    #[tokio::test]
    async fn base_target_diffs_from_merge_base() {
        let dir = init_repo();
        write(&dir, "base.txt", b"base\n");
        commit_all(&dir, "base");
        sh_git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        write(&dir, "feature.txt", b"feature\n");
        commit_all(&dir, "feature work");
        sh_git(dir.path(), &["checkout", "-q", "main"]);
        write(&dir, "main_only.txt", b"main\n");
        commit_all(&dir, "main moves on");
        sh_git(dir.path(), &["checkout", "-q", "feature"]);
        write(&dir, "wip.txt", b"wip\n");

        let set = load(&dir, DiffTarget::Base("main".into())).await;

        let paths: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["feature.txt", "wip.txt"]);
    }

    #[tokio::test]
    async fn commit_target_reads_content_from_the_commit() {
        let dir = init_repo();
        write(&dir, "f.txt", b"v1\n");
        let root = commit_all(&dir, "root");
        write(&dir, "f.txt", b"v1\nv2\n");
        sh_git(dir.path(), &["mv", "f.txt", "g.txt"]);
        let second = commit_all(&dir, "second");
        write(&dir, "g.txt", b"changed later\n");

        let set = load(&dir, DiffTarget::Commit(second)).await;
        let g = set.find("g.txt").expect("renamed file");
        assert_eq!(g.status, ChangeStatus::Renamed { from: "f.txt".into() });
        assert_eq!(g.new_content.as_deref(), Some("v1\nv2\n"));

        let root_set = load(&dir, DiffTarget::Commit(root)).await;
        let f = root_set.find("f.txt").expect("file in root commit");
        assert_eq!(f.status, ChangeStatus::Added);
        assert_eq!(f.new_content.as_deref(), Some("v1\n"));
    }

    #[tokio::test]
    async fn deleted_files_have_no_content() {
        let dir = init_repo();
        write(&dir, "gone.txt", b"bye\n");
        commit_all(&dir, "init");
        std::fs::remove_file(dir.path().join("gone.txt")).expect("remove");

        let set = load(&dir, DiffTarget::WorkingTree).await;
        let gone = set.find("gone.txt").expect("deleted file listed");
        assert_eq!(gone.status, ChangeStatus::Deleted);
        assert_eq!(gone.new_content, None);
    }

    #[test]
    fn rejects_refs_that_could_inject_options() {
        let root = PathBuf::from(".");
        for bad in ["", "--output=/tmp/x", "-p", "main branch", "a\tb", "x\u{7}"] {
            let result = LocalGit::new(root.clone(), DiffTarget::Base(bad.into()));
            assert!(matches!(result, Err(SourceError::InvalidInput(_))), "accepted {bad:?}");
            let result = LocalGit::new(root.clone(), DiffTarget::Commit(bad.into()));
            assert!(matches!(result, Err(SourceError::InvalidInput(_))), "accepted {bad:?}");
        }
        assert!(LocalGit::new(root.clone(), DiffTarget::Base("origin/main".into())).is_ok());
        assert!(LocalGit::new(root, DiffTarget::Commit("HEAD~1".into())).is_ok());
    }

    #[tokio::test]
    async fn unknown_ref_surfaces_git_error() {
        let dir = init_repo();
        write(&dir, "a", b"a\n");
        commit_all(&dir, "init");
        let result = source(&dir, DiffTarget::Commit("does-not-exist".into())).load().await;
        assert!(matches!(result, Err(SourceError::Git(msg)) if !msg.is_empty()));
    }

    #[tokio::test]
    async fn binary_content_is_flagged_without_content() {
        let dir = init_repo();
        write(&dir, "img.bin", b"\x00\x01\x02");
        write(&dir, "tracked_latin1.txt", b"ok\n");
        commit_all(&dir, "init");
        write(&dir, "img.bin", b"\x00\x01\x03");
        write(&dir, "tracked_latin1.txt", b"caf\xe9\n");
        write(&dir, "latin1.txt", b"caf\xe9\n");

        let set = load(&dir, DiffTarget::WorkingTree).await;
        let img = set.find("img.bin").expect("binary file listed");
        assert!(img.is_binary);
        assert_eq!(img.new_content, None);
        let latin = set.find("latin1.txt").expect("non-utf8 file listed");
        assert!(latin.is_binary);
        assert!(latin.hunks.is_empty());
        let tracked = set.find("tracked_latin1.txt").expect("modified non-utf8 file listed");
        assert!(tracked.is_binary);
        assert!(tracked.hunks.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_are_not_followed() {
        let outside = TempDir::new().expect("temp dir");
        std::fs::write(outside.path().join("secret"), "top secret\n").expect("write");
        let dir = init_repo();
        write(&dir, "a.txt", b"a\n");
        commit_all(&dir, "init");
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).expect("symlink");

        let set = load(&dir, DiffTarget::WorkingTree).await;
        assert!(set.find("link").is_none());
        sh_git(dir.path(), &["add", "link"]);
        let set = load(&dir, DiffTarget::WorkingTree).await;
        let link = set.find("link").expect("tracked symlink listed");
        assert_eq!(link.new_content, None);
    }

    #[tokio::test]
    async fn metadata_helpers_report_head_branch_and_message() {
        let dir = init_repo();
        write(&dir, "a", b"a\n");
        let sha = commit_all(&dir, "PROJ-123: add a\n\nLonger body.");
        let git = source(&dir, DiffTarget::WorkingTree);

        assert_eq!(git.head_commit().await.expect("query"), Some(sha.clone()));
        assert_eq!(git.current_branch().await.expect("query"), Some("main".into()));
        assert_eq!(git.commit_message(&sha).await.expect("message"), "PROJ-123: add a\n\nLonger body.");
        assert!(matches!(git.commit_message("--all").await, Err(SourceError::InvalidInput(_))));

        sh_git(dir.path(), &["checkout", "-q", "--detach"]);
        assert_eq!(git.current_branch().await.expect("query"), None);
    }

    #[tokio::test]
    async fn discover_root_finds_top_level_from_subdirectory() {
        let dir = init_repo();
        write(&dir, "sub/x", b"x\n");
        let root = discover_root(&dir.path().join("sub")).await.expect("inside repo");
        let expected = std::fs::canonicalize(dir.path()).expect("canonical");
        assert_eq!(std::fs::canonicalize(root).expect("canonical"), expected);

        let outside = TempDir::new().expect("temp dir");
        assert!(matches!(discover_root(outside.path()).await, Err(SourceError::Git(_))));
    }

    #[test]
    fn merge_untracked_replaces_shadowed_entries() {
        let entry = |path: &str, status| FileChange {
            path: path.into(),
            status,
            is_binary: false,
            hunks: Vec::new(),
            new_content: None,
        };
        let merged = merge_untracked(
            vec![entry("a", ChangeStatus::Deleted), entry("b", ChangeStatus::Modified)],
            vec![entry("a", ChangeStatus::Added)],
        );
        assert_eq!(merged, vec![entry("b", ChangeStatus::Modified), entry("a", ChangeStatus::Added)]);
    }

    #[tokio::test]
    async fn range_target_combines_several_commits() {
        let dir = init_repo();
        write(&dir, "a.txt", b"a1\n");
        let base = commit_all(&dir, "base");
        write(&dir, "a.txt", b"a1\na2\n");
        write(&dir, "b.txt", b"b\n");
        commit_all(&dir, "second");
        std::fs::remove_file(dir.path().join("b.txt")).expect("remove");
        write(&dir, "c.txt", b"c\n");
        let head = commit_all(&dir, "third");
        write(&dir, "a.txt", b"working tree only\n");

        let set = load(&dir, DiffTarget::Range { base, head }).await;

        let paths: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "c.txt"], "b.txt was added and removed inside the range");
        assert_eq!(set.find("a.txt").expect("a").new_content.as_deref(), Some("a1\na2\n"));
        assert_eq!(set.find("c.txt").expect("c").status, ChangeStatus::Added);
    }

    #[test]
    fn range_target_validates_both_refs() {
        let root = PathBuf::from(".");
        let range = |base: &str, head: &str| DiffTarget::Range { base: base.into(), head: head.into() };
        assert!(matches!(LocalGit::new(root.clone(), range("-p", "HEAD")), Err(SourceError::InvalidInput(_))));
        assert!(matches!(LocalGit::new(root.clone(), range("HEAD~2", "--all")), Err(SourceError::InvalidInput(_))));
        assert!(LocalGit::new(root, range("HEAD~2", "HEAD")).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repository_fsmonitor_is_never_run() {
        let dir = init_repo();
        write(&dir, "a.txt", b"a\n");
        commit_all(&dir, "init");
        let scripts = TempDir::new().expect("temp dir");
        let marker = scripts.path().join("fsmonitor-ran");
        let script = scripts.path().join("fsmonitor.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).expect("write script");
        let executable = <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755);
        std::fs::set_permissions(&script, executable).expect("chmod");
        sh_git(dir.path(), &["config", "core.fsmonitor", script.to_str().expect("utf-8 path")]);
        write(&dir, "a.txt", b"b\n");
        write(&dir, "new.txt", b"n\n");

        let git = source(&dir, DiffTarget::WorkingTree);
        let set = git.load().await.expect("load succeeds");
        git.head_commit().await.expect("head");

        assert_eq!(set.files.len(), 2);
        assert!(!marker.exists(), "fsmonitor hook from the repository config was executed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn path_with_newline_is_reported_verbatim() {
        let dir = init_repo();
        write(&dir, "odd\nname.txt", b"one\n");
        write(&dir, "plain.txt", b"p\n");
        commit_all(&dir, "init");
        write(&dir, "odd\nname.txt", b"two\n");
        write(&dir, "plain.txt", b"q\n");

        let set = load(&dir, DiffTarget::WorkingTree).await;

        assert_eq!(set.files.len(), 2);
        assert_eq!(set.find("odd\nname.txt").expect("odd path").new_content.as_deref(), Some("two\n"));
        assert!(set.find("plain.txt").is_some());
    }

    #[test]
    fn retain_listed_drops_entries_missing_from_the_name_list() {
        let entry = |path: &str| FileChange {
            path: path.into(),
            status: ChangeStatus::Modified,
            is_binary: false,
            hunks: Vec::new(),
            new_content: None,
        };
        let kept = retain_listed(vec![entry("a"), entry("b\nc"), entry("forged")], "a\0b\nc\0");
        assert_eq!(kept, vec![entry("a"), entry("b\nc")]);
    }

    #[tokio::test]
    async fn oversized_untracked_file_is_not_read() {
        let dir = init_repo();
        write(&dir, "a.txt", b"a\n");
        commit_all(&dir, "init");
        let size = usize::try_from(MAX_WORKING_FILE_BYTES).expect("fits") + 1;
        write(&dir, "huge.txt", &vec![b'x'; size]);

        let set = load(&dir, DiffTarget::WorkingTree).await;
        let huge = set.find("huge.txt").expect("listed");
        assert!(huge.is_binary);
        assert!(huge.hunks.is_empty());
        assert_eq!(huge.new_content, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn files_behind_a_symlinked_directory_are_not_read() {
        let outside = TempDir::new().expect("temp dir");
        std::fs::write(outside.path().join("secret.txt"), "top secret\n").expect("write");
        let dir = init_repo();
        write(&dir, "linked/secret.txt", b"old\n");
        commit_all(&dir, "init");
        std::fs::remove_dir_all(dir.path().join("linked")).expect("remove dir");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).expect("symlink");
        let git = source(&dir, DiffTarget::WorkingTree);
        let root = std::fs::canonicalize(dir.path()).expect("canonical");

        let read = git.read_working_file(&root, "linked/secret.txt").await.expect("read");
        assert!(matches!(read, WorkingFile::Absent));
        let set = git.load().await.expect("load succeeds");
        assert!(set.files.iter().all(|f| f.new_content.as_deref() != Some("top secret\n")));
    }

    #[tokio::test]
    async fn read_file_at_reads_committed_text() {
        let dir = init_repo();
        write(&dir, ".qgate.toml", b"strict = true\n");
        write(&dir, "bin.dat", b"caf\xe9\n");
        write(&dir, "sub/x.txt", b"x\n");
        let first = commit_all(&dir, "first");
        write(&dir, ".qgate.toml", b"strict = false\n");
        let second = commit_all(&dir, "weaken");
        let git = source(&dir, DiffTarget::WorkingTree);

        assert_eq!(git.read_file_at(&first, ".qgate.toml").await.unwrap().as_deref(), Some("strict = true\n"));
        assert_eq!(git.read_file_at(&second, ".qgate.toml").await.unwrap().as_deref(), Some("strict = false\n"));
        assert_eq!(git.read_file_at(&first, "missing.toml").await.unwrap(), None);
        assert_eq!(git.read_file_at(&first, "sub").await.unwrap(), None, "a directory is not a file");
        assert_eq!(git.read_file_at(&first, "bin.dat").await.unwrap(), None, "not UTF-8");
        assert_eq!(git.read_file_at(EMPTY_TREE, ".qgate.toml").await.unwrap(), None);
        assert!(matches!(git.read_file_at("nope", ".qgate.toml").await, Err(SourceError::Git(_))));
    }

    #[test]
    fn read_file_at_rejects_unsafe_paths() {
        for bad in ["", "../x", "a/../b", "/etc/passwd", "-p", "./a", "a\nb", "\\x"] {
            assert!(matches!(validate_repo_path(bad), Err(SourceError::InvalidInput(_))), "accepted {bad:?}");
        }
        for good in [".qgate.toml", "config/qgate.toml", "a b.txt"] {
            assert!(validate_repo_path(good).is_ok(), "rejected {good:?}");
        }
    }

    #[tokio::test]
    async fn read_file_at_validates_rev() {
        let git = LocalGit::new(PathBuf::from("."), DiffTarget::WorkingTree).expect("valid");
        assert!(matches!(git.read_file_at("--all", "a").await, Err(SourceError::InvalidInput(_))));
        assert!(matches!(git.read_file_at("HEAD", "../a").await, Err(SourceError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn parent_of_returns_first_parent_or_none_for_root() {
        let dir = init_repo();
        write(&dir, "a", b"a\n");
        let root = commit_all(&dir, "root");
        write(&dir, "a", b"b\n");
        let child = commit_all(&dir, "child");
        let git = source(&dir, DiffTarget::WorkingTree);

        assert_eq!(git.parent_of(&child).await.unwrap(), Some(root.clone()));
        assert_eq!(git.parent_of("HEAD").await.unwrap(), Some(root.clone()));
        assert_eq!(git.parent_of(&root).await.unwrap(), None);
        assert!(matches!(git.parent_of("-x").await, Err(SourceError::InvalidInput(_))));
        assert!(matches!(git.parent_of("nope").await, Err(SourceError::Git(_))));
    }

    #[tokio::test]
    async fn merge_base_and_is_ancestor_follow_history() {
        let dir = init_repo();
        write(&dir, "a", b"a\n");
        let base = commit_all(&dir, "base");
        sh_git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        write(&dir, "b", b"b\n");
        let feature = commit_all(&dir, "feature");
        sh_git(dir.path(), &["checkout", "-q", "main"]);
        write(&dir, "c", b"c\n");
        let main_tip = commit_all(&dir, "main moves on");
        let git = source(&dir, DiffTarget::WorkingTree);

        assert_eq!(git.merge_base(&main_tip, &feature).await.unwrap(), Some(base.clone()));
        assert!(git.is_ancestor(&base, &feature).await.unwrap());
        assert!(git.is_ancestor(&feature, &feature).await.unwrap(), "a commit is its own ancestor");
        assert!(!git.is_ancestor(&feature, &main_tip).await.unwrap());
        assert!(matches!(git.merge_base("-x", &feature).await, Err(SourceError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn merge_base_is_none_for_unrelated_histories() {
        let dir = init_repo();
        write(&dir, "a", b"a\n");
        let first = commit_all(&dir, "first");
        // An orphan branch shares no history with the first commit.
        sh_git(dir.path(), &["checkout", "-q", "--orphan", "other"]);
        sh_git(dir.path(), &["rm", "-rfq", "--cached", "."]);
        write(&dir, "z", b"z\n");
        let orphan = commit_all(&dir, "orphan root");
        let git = source(&dir, DiffTarget::WorkingTree);

        assert_eq!(git.merge_base(&first, &orphan).await.unwrap(), None);
    }

    #[tokio::test]
    async fn object_id_at_and_tree_id_at_are_content_addressed() {
        let dir = init_repo();
        write(&dir, "same.txt", b"same\n");
        write(&dir, "pkg/x.txt", b"x\n");
        let first = commit_all(&dir, "first");
        write(&dir, "other.txt", b"other\n");
        let second = commit_all(&dir, "second");
        let git = source(&dir, DiffTarget::WorkingTree);

        // Unchanged file keeps its blob id across commits; a new file is absent from the first commit.
        let blob_first = git.object_id_at(&first, "same.txt").await.unwrap();
        assert!(blob_first.is_some());
        assert_eq!(git.object_id_at(&second, "same.txt").await.unwrap(), blob_first);
        assert_eq!(git.object_id_at(&first, "other.txt").await.unwrap(), None);

        // Directory object ids are trees; a file is not a tree; the root tree resolves for an empty path.
        let dir_tree = git.tree_id_at(&first, "pkg").await.unwrap();
        assert_eq!(git.object_id_at(&first, "pkg").await.unwrap(), dir_tree);
        assert!(dir_tree.is_some());
        assert_eq!(git.tree_id_at(&first, "same.txt").await.unwrap(), None, "a file is not a tree");
        assert_eq!(git.tree_id_at(&first, "missing").await.unwrap(), None);
        assert!(git.tree_id_at(&first, "").await.unwrap().is_some(), "root tree");
        assert!(matches!(git.object_id_at(&first, "../x").await, Err(SourceError::InvalidInput(_))));
    }
}
