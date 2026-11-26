use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::*;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn repo_with(files: &[(&str, &str)]) -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    for (path, content) in files {
        write(repo.path(), path, content);
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "first"]);
    repo
}

fn write(root: &Path, rel: &str, content: &str) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

#[tokio::test]
async fn snapshot_carries_uncommitted_edits_new_files_and_deletions() {
    let repo = repo_with(&[("a.txt", "committed"), ("keep.txt", "same"), ("gone.txt", "bye"), ("dir/b.txt", "old")]);
    write(repo.path(), "a.txt", "edited");
    write(repo.path(), "dir/b.txt", "staged");
    git(repo.path(), &["add", "dir/b.txt"]);
    write(repo.path(), "new/c.txt", "untracked");
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    let parent = tempfile::tempdir().unwrap();

    let snapshot = export_working_tree(repo.path(), parent.path()).await.unwrap();

    let root = snapshot.path();
    assert_eq!(read(root, "a.txt").as_deref(), Some("edited"));
    assert_eq!(read(root, "dir/b.txt").as_deref(), Some("staged"));
    assert_eq!(read(root, "new/c.txt").as_deref(), Some("untracked"));
    assert_eq!(read(root, "keep.txt").as_deref(), Some("same"));
    assert_eq!(read(root, "gone.txt"), None);
    assert!(!root.join(".git").exists(), "the snapshot is not a checkout of the repository");
    assert_eq!(read(repo.path(), "a.txt").as_deref(), Some("edited"), "the repository is untouched");
}

#[tokio::test]
async fn ignored_files_are_not_copied() {
    let repo = repo_with(&[(".gitignore", "target/\n*.log\n"), ("a.txt", "x")]);
    write(repo.path(), "target/big.bin", "build output");
    write(repo.path(), "run.log", "noise");
    let parent = tempfile::tempdir().unwrap();

    let snapshot = export_working_tree(repo.path(), parent.path()).await.unwrap();

    assert!(!snapshot.path().join("target").exists());
    assert!(!snapshot.path().join("run.log").exists());
}

#[tokio::test]
async fn a_repository_without_commits_is_an_error() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    let parent = tempfile::tempdir().unwrap();

    assert!(export_working_tree(repo.path(), parent.path()).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_in_the_working_tree_are_not_followed() {
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.txt", "host secret");
    let repo = repo_with(&[("a.txt", "x")]);
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), repo.path().join("leak.txt")).unwrap();
    std::os::unix::fs::symlink(outside.path(), repo.path().join("linkdir")).unwrap();
    let parent = tempfile::tempdir().unwrap();

    let snapshot = export_working_tree(repo.path(), parent.path()).await.unwrap();

    assert!(std::fs::symlink_metadata(snapshot.path().join("leak.txt")).is_err());
    assert!(std::fs::symlink_metadata(snapshot.path().join("linkdir")).is_err());
}

#[cfg(unix)]
#[test]
fn overlay_never_writes_through_a_committed_symlinked_directory() {
    let repo = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(repo.path(), "out/x.txt", "payload");
    std::os::unix::fs::symlink(outside.path(), target.path().join("out")).unwrap();

    overlay(repo.path(), target.path(), &[PathBuf::from("out/x.txt")]).unwrap();

    assert!(!outside.path().join("x.txt").exists());
}

#[cfg(unix)]
#[test]
fn overlay_replaces_a_committed_file_symlink_instead_of_writing_through_it() {
    let repo = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "victim.txt", "original");
    write(repo.path(), "link.txt", "new content");
    std::os::unix::fs::symlink(outside.path().join("victim.txt"), target.path().join("link.txt")).unwrap();

    overlay(repo.path(), target.path(), &[PathBuf::from("link.txt")]).unwrap();

    assert_eq!(read(outside.path(), "victim.txt").as_deref(), Some("original"));
    assert_eq!(read(target.path(), "link.txt").as_deref(), Some("new content"));
}

#[test]
fn only_plain_relative_paths_are_overlaid() {
    assert!(is_plain_relative(Path::new("src/main.go")));
    for bad in ["", "../x", "a/../../x", "/etc/passwd", "./a"] {
        assert!(!is_plain_relative(Path::new(bad)), "{bad:?}");
    }
}
