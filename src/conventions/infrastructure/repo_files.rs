//! Read-only repository access: tracked files via `git ls-files` (hardened like the review
//! context's git calls), falling back to a bounded directory walk outside a git checkout.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use crate::conventions::application::{ConventionsError, RepoReader};
use crate::conventions::domain::sources::is_ignored_path;

/// Files listed at most (protects against monorepos with millions of files).
const MAX_FILES: usize = 50_000;
/// Directory levels walked by the fallback.
const MAX_WALK_DEPTH: usize = 12;
/// Same hardening as the review context: the repository's own config must not run programs.
const GIT_ARGS: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "protocol.allow=never",
    "-c",
    "core.quotepath=off",
];
const GIT_ENV: &[(&str, &str)] = &[("GIT_TERMINAL_PROMPT", "0"), ("GIT_OPTIONAL_LOCKS", "0"), ("GIT_PAGER", "cat")];

#[derive(Debug, Default, Clone, Copy)]
pub struct FsRepoReader;

fn repository(err: impl std::fmt::Display) -> ConventionsError {
    ConventionsError::Repository(err.to_string())
}

fn git_ls_files(root: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .args(GIT_ARGS)
        .args(["ls-files", "-z"])
        .envs(GIT_ENV.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.split('\0').filter(|p| !p.is_empty()).take(MAX_FILES).map(str::to_string).collect())
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) -> std::io::Result<()> {
    if depth > MAX_WALK_DEPTH {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_FILES {
            return Ok(());
        }
        let file_type = entry.file_type()?;
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(root) else { continue };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if is_ignored_path(&relative) {
            continue;
        }
        if file_type.is_dir() {
            walk(root, &path, depth + 1, out)?;
        } else if file_type.is_file() {
            out.push(relative);
        }
    }
    Ok(())
}

/// Only plain relative paths inside the root (no `..`, no absolute paths).
fn is_safe_relative(path: &str) -> bool {
    !path.is_empty() && Path::new(path).components().all(|c| matches!(c, Component::Normal(_)))
}

impl RepoReader for FsRepoReader {
    fn list_files(&self, root: &Path) -> Result<Vec<String>, ConventionsError> {
        if !root.is_dir() {
            return Err(repository(format!("{} is not a directory", root.display())));
        }
        if let Some(files) = git_ls_files(root) {
            return Ok(files);
        }
        let mut files = Vec::new();
        walk(root, root, 0, &mut files).map_err(repository)?;
        Ok(files)
    }

    fn read_capped(&self, root: &Path, path: &str, cap: usize) -> Result<Option<String>, ConventionsError> {
        if !is_safe_relative(path) {
            return Ok(None);
        }
        // Symlinks (the file or any parent directory) could point outside the repository.
        let Some(full) = resolve_without_symlinks(root, path) else { return Ok(None) };
        let file = File::open(&full).map_err(repository)?;
        let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
        file.take(cap as u64).read_to_end(&mut bytes).map_err(repository)?;
        if bytes.contains(&0) {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// `root/path` when every component below `root` is a real directory and the last one a regular file.
fn resolve_without_symlinks(root: &Path, path: &str) -> Option<std::path::PathBuf> {
    let mut full = root.to_path_buf();
    let mut components = Path::new(path).components().peekable();
    while let Some(component) = components.next() {
        full.push(component);
        let kind = std::fs::symlink_metadata(&full).ok()?.file_type();
        let expected = if components.peek().is_some() { kind.is_dir() } else { kind.is_file() };
        if !expected {
            return None;
        }
    }
    Some(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_directory_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("CLAUDE.md"), "secret").unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join(".claude")).unwrap();
        assert_eq!(FsRepoReader.read_capped(repo.path(), ".claude/CLAUDE.md", 100).unwrap(), None);
    }

    #[test]
    fn walks_without_git_and_reads_capped_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/domain")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("src/domain/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(root.join("node_modules/x/b.js"), "x").unwrap();
        std::fs::write(root.join("bin.dat"), [0u8, 1, 2]).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "0123456789").unwrap();
        let reader = FsRepoReader;
        let files = reader.list_files(root).unwrap();
        assert_eq!(files, ["CLAUDE.md", "bin.dat", "src/domain/a.rs"]);
        assert_eq!(reader.read_capped(root, "CLAUDE.md", 4).unwrap().as_deref(), Some("0123"));
        assert_eq!(reader.read_capped(root, "bin.dat", 10).unwrap(), None);
        assert_eq!(reader.read_capped(root, "../etc/passwd", 10).unwrap(), None);
        assert_eq!(reader.read_capped(root, "/etc/passwd", 10).unwrap(), None);
        assert_eq!(reader.read_capped(root, "missing.md", 10).unwrap(), None);
        assert!(reader.list_files(&root.join("nope")).is_err());
    }

    #[test]
    fn lists_tracked_files_in_a_git_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| Command::new("git").args(args).current_dir(root).output().unwrap();
        if !git(&["init", "-q"]).status.success() {
            return; // git not installed
        }
        std::fs::write(root.join("tracked.rs"), "fn t() {}\n").unwrap();
        std::fs::write(root.join("untracked.rs"), "fn u() {}\n").unwrap();
        git(&["add", "tracked.rs"]);
        assert_eq!(FsRepoReader.list_files(root).unwrap(), ["tracked.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link.md")).unwrap();
        assert_eq!(FsRepoReader.read_capped(dir.path(), "link.md", 10).unwrap(), None);
    }
}
