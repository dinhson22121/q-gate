use std::fs;
use std::path::Path;

use futures::future::BoxFuture;

use super::git::{git_bytes, git_text};
use crate::eval::application::{CaseWorkspace, EvalError, LoadedCase, MaterializedRepo};

const WORKSPACE_PREFIX: &str = "qgate-eval-";
const REPO_DIR: &str = "repo";
const PATCH_FILE: &str = "change.patch";
const BASE_BRANCH: &str = "main";
const CHANGE_BRANCH: &str = "change";

pub struct GitWorkspace;

impl CaseWorkspace for GitWorkspace {
    fn materialize<'a>(&'a self, case: &'a LoadedCase) -> BoxFuture<'a, Result<MaterializedRepo, EvalError>> {
        Box::pin(build(case))
    }
}

async fn build(case: &LoadedCase) -> Result<MaterializedRepo, EvalError> {
    let dir = tempfile::Builder::new()
        .prefix(WORKSPACE_PREFIX)
        .tempdir()
        .map_err(|e| EvalError::Workspace(format!("cannot create a temp dir: {e}")))?;
    let root = dir.path().join(REPO_DIR);
    copy_tree(&case.base_dir, &root)?;
    let patch = dir.path().join(PATCH_FILE);
    fs::write(&patch, &case.patch).map_err(|e| EvalError::Workspace(format!("cannot write the patch: {e}")))?;

    git_bytes(&root, &["init", "-q", "-b", BASE_BRANCH]).await?;
    git_bytes(&root, &["add", "-A"]).await?;
    git_bytes(&root, &["commit", "-q", "--allow-empty", "-m", "base"]).await?;
    let base = git_text(&root, &["rev-parse", "HEAD"]).await?;
    git_bytes(&root, &["checkout", "-q", "-b", CHANGE_BRANCH]).await?;
    let patch_arg = patch.to_string_lossy().into_owned();
    git_bytes(&root, &["apply", "--whitespace=nowarn", &patch_arg]).await.map_err(|e| {
        EvalError::Workspace(format!("case '{}': the patch does not apply to base/: {e}", case.case.id))
    })?;
    git_bytes(&root, &["add", "-A"]).await?;
    git_bytes(&root, &["commit", "-q", "--allow-empty", "-m", "change"]).await?;
    let head = git_text(&root, &["rev-parse", "HEAD"]).await?;
    Ok(MaterializedRepo { root, base, head, lease: Box::new(dir) })
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), EvalError> {
    let fail = |action: &str, path: &Path, e: std::io::Error| {
        EvalError::Workspace(format!("cannot {action} {}: {e}", path.display()))
    };
    fs::create_dir_all(to).map_err(|e| fail("create", to, e))?;
    for entry in fs::read_dir(from).map_err(|e| fail("read", from, e))? {
        let entry = entry.map_err(|e| fail("read", from, e))?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| fail("inspect", &source, e))?;
        if kind.is_symlink() {
            return Err(EvalError::Workspace(format!(
                "{} is a symlink; case trees must not contain symlinks",
                source.display()
            )));
        }
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            fs::copy(&source, &target).map_err(|e| fail("copy", &source, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::domain::case::parse_case;

    const CASE: &str =
        "id = \"ws\"\nlanguage = \"go\"\n[[expected]]\nfile = \"a.txt\"\nline = 1\ncategory = \"security\"\n";
    const PATCH: &str = "diff --git a/a.txt b/a.txt\nindex 3b18e51..cc628cc 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n hello world\n+second line\n";

    fn loaded(base: &Path, patch: &str) -> LoadedCase {
        LoadedCase {
            case: parse_case(CASE).unwrap(),
            base_dir: base.to_path_buf(),
            patch: patch.into(),
            content_hash: String::new(),
        }
    }

    fn base_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b.txt"), "b\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn materializes_base_and_change_as_two_commits() {
        let base = base_tree();

        let repo = GitWorkspace.materialize(&loaded(base.path(), PATCH)).await.unwrap();

        assert_ne!(repo.base, repo.head);
        assert_eq!(fs::read_to_string(repo.root.join("a.txt")).unwrap(), "hello world\nsecond line\n");
        assert_eq!(git_text(&repo.root, &["show", &format!("{}:a.txt", repo.base)]).await.unwrap(), "hello world");
        assert_eq!(git_text(&repo.root, &["rev-parse", &format!("{}^", repo.head)]).await.unwrap(), repo.base);
        assert_eq!(git_text(&repo.root, &["status", "--porcelain"]).await.unwrap(), "");
        assert!(repo.root.join("sub/b.txt").is_file());
    }

    #[tokio::test]
    async fn the_workspace_disappears_when_the_lease_is_dropped() {
        let base = base_tree();
        let repo = GitWorkspace.materialize(&loaded(base.path(), PATCH)).await.unwrap();
        let root = repo.root.clone();

        drop(repo);

        assert!(!root.exists());
    }

    #[tokio::test]
    async fn a_patch_that_does_not_apply_names_the_case() {
        let base = base_tree();
        let bad = PATCH.replace("hello world", "different text");

        let error = GitWorkspace.materialize(&loaded(base.path(), &bad)).await.err().unwrap();

        assert!(
            matches!(&error, EvalError::Workspace(m) if m.contains("case 'ws'") && m.contains("does not apply")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_patch_cannot_write_outside_the_workspace() {
        let base = base_tree();
        let escaping = "diff --git a/../evil.txt b/../evil.txt\nnew file mode 100644\n--- /dev/null\n+++ b/../evil.txt\n@@ -0,0 +1 @@\n+x\n";

        let result = GitWorkspace.materialize(&loaded(base.path(), escaping)).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn symlinks_in_the_base_tree_are_refused() {
        let base = base_tree();
        std::os::unix::fs::symlink("/etc/passwd", base.path().join("link")).unwrap();

        let error = GitWorkspace.materialize(&loaded(base.path(), PATCH)).await.err().unwrap();

        assert!(matches!(&error, EvalError::Workspace(m) if m.contains("symlink")));
    }
}
