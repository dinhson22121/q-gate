use std::path::Path;

use futures::future::BoxFuture;

use super::git::{is_safe_rev, user_repo_bytes as git_bytes, user_repo_text as git_text};
use crate::eval::application::{BaseFile, ChangeExporter, EvalError, ExportedChange};
use crate::eval::domain::case::is_safe_relative_path;

const MAX_BASE_FILES: usize = 500;
const MAX_BASE_BYTES: usize = 5 * 1024 * 1024;
const REGULAR_FILE_MODES: [&str; 2] = ["100644", "100755"];
const FALLBACK_BASES: [&str; 2] = ["main", "master"];

pub struct GitChangeExporter;

impl ChangeExporter for GitChangeExporter {
    fn export<'a>(
        &'a self,
        repo: &'a Path,
        commit: &'a str,
        base_branch: &'a str,
    ) -> BoxFuture<'a, Result<ExportedChange, EvalError>> {
        Box::pin(export(repo, commit, base_branch))
    }
}

fn import_error(message: impl Into<String>) -> EvalError {
    EvalError::Import(message.into())
}

async fn export(repo: &Path, commit: &str, base_branch: &str) -> Result<ExportedChange, EvalError> {
    if !is_safe_rev(commit) || (!base_branch.is_empty() && !is_safe_rev(base_branch)) {
        return Err(import_error("the commit or base branch is not a plain revision name"));
    }
    let head = git_text(repo, &["rev-parse", "--verify", &format!("{commit}^{{commit}}")])
        .await
        .map_err(|e| import_error(format!("commit {commit} is not in {}: {}", repo.display(), reason(&e))))?;
    let base = merge_base(repo, &head, base_branch).await?;
    let base_files = read_tree(repo, &base).await?;
    let patch = git_bytes(
        repo,
        &["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--src-prefix=a/", "--dst-prefix=b/", &base, &head],
    )
    .await?;
    if patch.is_empty() {
        return Err(import_error(format!("commit {commit} introduces no change relative to {base}")));
    }
    let patch = String::from_utf8(patch).map_err(|_| import_error("the change is not valid UTF-8 text"))?;
    Ok(ExportedChange { base_files, patch })
}

fn reason(error: &EvalError) -> String {
    match error {
        EvalError::Workspace(message) => message.clone(),
        other => other.to_string(),
    }
}

async fn merge_base(repo: &Path, head: &str, base_branch: &str) -> Result<String, EvalError> {
    let origin = format!("origin/{base_branch}");
    let candidates = [base_branch, origin.as_str()].into_iter().chain(FALLBACK_BASES).filter(|c| !c.is_empty());
    for candidate in candidates {
        let Ok(base) = git_text(repo, &["merge-base", candidate, head]).await else { continue };
        if base != head {
            return Ok(base);
        }
    }
    git_text(repo, &["rev-parse", "--verify", &format!("{head}^")])
        .await
        .map_err(|_| import_error("cannot find where the change branched off, and the commit has no parent"))
}

async fn read_tree(repo: &Path, base: &str) -> Result<Vec<BaseFile>, EvalError> {
    let listing = git_bytes(repo, &["ls-tree", "-r", "-z", base]).await?;
    let mut files = Vec::new();
    let mut total = 0;
    for record in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let Some((meta, path)) = record.split_once('\t') else { continue };
        let mut parts = meta.split_whitespace();
        let (mode, kind) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
        if kind != "blob" || !REGULAR_FILE_MODES.contains(&mode) {
            continue;
        }
        if !is_safe_relative_path(path) {
            return Err(import_error(format!("unsafe path '{path}' in the base tree")));
        }
        if files.len() >= MAX_BASE_FILES {
            return Err(import_error(format!(
                "the base tree has more than {MAX_BASE_FILES} files; too large for a case"
            )));
        }
        let bytes = git_bytes(repo, &["cat-file", "blob", &format!("{base}:{path}")]).await?;
        total += bytes.len();
        if total > MAX_BASE_BYTES {
            return Err(import_error(format!(
                "the base tree is larger than {} MiB; too large for a case",
                MAX_BASE_BYTES >> 20
            )));
        }
        files.push(BaseFile { path: path.to_string(), bytes });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::application::CaseWorkspace;

    async fn git(dir: &Path, args: &[&str]) -> String {
        git_text(dir, args).await.unwrap()
    }

    async fn repo_with_feature_branch() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]).await;
        std::fs::write(root.join("a.go"), "package a\n").unwrap();
        std::fs::create_dir(root.join("pkg")).unwrap();
        std::fs::write(root.join("pkg/b.go"), "package b\n").unwrap();
        git(root, &["add", "-A"]).await;
        git(root, &["commit", "-q", "-m", "base"]).await;
        let base = git(root, &["rev-parse", "HEAD"]).await;
        git(root, &["checkout", "-q", "-b", "feature"]).await;
        std::fs::write(root.join("a.go"), "package a\n\nfunc f() {}\n").unwrap();
        git(root, &["commit", "-q", "-am", "feature one"]).await;
        std::fs::write(root.join("c.go"), "package c\n").unwrap();
        git(root, &["add", "-A"]).await;
        git(root, &["commit", "-q", "-m", "feature two"]).await;
        let head = git(root, &["rev-parse", "HEAD"]).await;
        (dir, base, head)
    }

    #[tokio::test]
    async fn a_missing_commit_reports_the_git_reason() {
        let (dir, _, _) = repo_with_feature_branch().await;

        let error = GitChangeExporter.export(dir.path(), &"0".repeat(40), "main").await.unwrap_err();

        let EvalError::Import(message) = error else { panic!("{error}") };
        assert!(message.contains("is not in") && message.contains("git rev-parse"), "{message}");
    }

    #[test]
    fn the_user_repo_is_trusted_by_its_canonical_path() {
        let dir = tempfile::tempdir().unwrap();

        let args = crate::eval::infrastructure::git::safe_directory_args(dir.path());

        assert_eq!(args[0], "-c");
        let expected = dir.path().canonicalize().unwrap();
        assert_eq!(args[1], format!("safe.directory={}", expected.display()));
    }

    #[tokio::test]
    async fn exports_the_tree_at_the_merge_base_and_the_whole_branch_diff() {
        let (dir, _, head) = repo_with_feature_branch().await;

        let change = GitChangeExporter.export(dir.path(), &head, "main").await.unwrap();

        let paths: Vec<&str> = change.base_files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.go", "pkg/b.go"]);
        assert_eq!(change.base_files[0].bytes, b"package a\n");
        assert!(change.patch.contains("+func f() {}") && change.patch.contains("b/c.go"));
    }

    #[tokio::test]
    async fn the_patch_applies_to_the_exported_base() {
        let (dir, _, head) = repo_with_feature_branch().await;
        let change = GitChangeExporter.export(dir.path(), &head, "main").await.unwrap();
        let loaded_dir = tempfile::tempdir().unwrap();
        for file in &change.base_files {
            let target = loaded_dir.path().join(&file.path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, &file.bytes).unwrap();
        }
        let case = crate::eval::application::LoadedCase {
            case: crate::eval::domain::case::parse_case(
                "id = \"x\"\nlanguage = \"go\"\n[[expected]]\nfile = \"a.go\"\nline = 1\ncategory = \"security\"\n",
            )
            .unwrap(),
            base_dir: loaded_dir.path().to_path_buf(),
            patch: change.patch,
            content_hash: String::new(),
        };

        let repo = crate::eval::infrastructure::GitWorkspace.materialize(&case).await.unwrap();

        assert!(repo.root.join("c.go").is_file());
    }

    #[tokio::test]
    async fn a_commit_on_the_base_branch_falls_back_to_its_parent() {
        let (dir, base, _) = repo_with_feature_branch().await;
        git(dir.path(), &["checkout", "-q", "main"]).await;
        std::fs::write(dir.path().join("d.go"), "package d\n").unwrap();
        git(dir.path(), &["add", "-A"]).await;
        git(dir.path(), &["commit", "-q", "-m", "on main"]).await;
        let tip = git(dir.path(), &["rev-parse", "HEAD"]).await;

        let change = GitChangeExporter.export(dir.path(), &tip, "main").await.unwrap();

        assert!(change.patch.contains("b/d.go") && !change.patch.contains("a.go"));
        assert_eq!(base, git(dir.path(), &["rev-parse", &format!("{tip}^")]).await);
    }

    #[tokio::test]
    async fn unknown_commits_and_unsafe_revisions_are_import_errors() {
        let (dir, _, head) = repo_with_feature_branch().await;

        let missing = GitChangeExporter.export(dir.path(), "deadbeefdeadbeef", "main").await;
        let option_like = GitChangeExporter.export(dir.path(), "--output=/tmp/x", "main").await;
        let bad_base = GitChangeExporter.export(dir.path(), &head, "--upload-pack=x").await;

        for result in [missing, option_like, bad_base] {
            assert!(matches!(result, Err(EvalError::Import(_))));
        }
    }

    #[tokio::test]
    async fn a_root_commit_has_no_base() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]).await;
        std::fs::write(dir.path().join("a.go"), "x").unwrap();
        git(dir.path(), &["add", "-A"]).await;
        git(dir.path(), &["commit", "-q", "-m", "root"]).await;
        let root = git(dir.path(), &["rev-parse", "HEAD"]).await;

        let result = GitChangeExporter.export(dir.path(), &root, "main").await;

        assert!(matches!(result, Err(EvalError::Import(m)) if m.contains("no parent")));
    }

    #[test]
    fn revision_names_reject_option_and_range_syntax() {
        assert!(is_safe_rev("origin/main") && is_safe_rev("feature/KAN-1"));
        for bad in ["", "-x", "a b", "a^b", "a:b", "a~1", "a\nb"] {
            assert!(!is_safe_rev(bad), "{bad:?}");
        }
    }
}
