use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::review::infrastructure::local_git::{GIT_HARDENING_ARGS, GIT_HARDENING_ENV};
use crate::review::infrastructure::snapshot::{Snapshot, export_commit, neutralised_filters, resolve_commit};

const GIT: &str = "git";

pub async fn export_working_tree(repo: &Path, parent: &Path) -> Result<Snapshot> {
    let head = resolve_commit(repo, "HEAD").await?;
    let snapshot = export_commit(repo, &head, parent).await?;
    let (repo, target) = (repo.to_path_buf(), snapshot.path().to_path_buf());
    tokio::task::spawn_blocking(move || {
        let changed = working_tree_changes(&repo)?;
        overlay(&repo, &target, &changed)
    })
    .await??;
    Ok(snapshot)
}

fn working_tree_changes(repo: &Path) -> Result<Vec<PathBuf>> {
    let filters = neutralised_filters(repo)?;
    let list = |args: &[&str]| -> Result<Vec<PathBuf>> {
        let output = Command::new(GIT)
            .args(GIT_HARDENING_ARGS)
            .args(&filters)
            .args(args)
            .envs(GIT_HARDENING_ENV.iter().copied())
            .current_dir(repo)
            .stdin(Stdio::null())
            .output()
            .context("failed to run git")?;
        if !output.status.success() {
            bail!("git {} failed: {}", args[0], String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).split('\0').filter(|p| !p.is_empty()).map(PathBuf::from).collect())
    };
    let mut paths = list(&["diff", "--name-only", "-z", "--no-renames", "--no-ext-diff", "HEAD", "--"])?;
    paths.extend(list(&["ls-files", "-z", "--others", "--exclude-standard"])?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn overlay(repo: &Path, target: &Path, paths: &[PathBuf]) -> Result<()> {
    for rel in paths {
        if !is_plain_relative(rel) || has_symlink_ancestor(repo, rel) || has_symlink_ancestor(target, rel) {
            continue;
        }
        let (source, dest) = (repo.join(rel), target.join(rel));
        match std::fs::symlink_metadata(&source) {
            Ok(meta) if meta.is_file() => copy_file(&source, &dest)?,
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => remove_file(&dest)?,
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", source.display())),
        }
    }
    Ok(())
}

fn copy_file(source: &Path, dest: &Path) -> Result<()> {
    remove_file(dest)?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::copy(source, dest).with_context(|| format!("cannot copy {}", source.display()))?;
    Ok(())
}

fn remove_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_dir() => {
            std::fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

fn is_plain_relative(rel: &Path) -> bool {
    rel.components().next().is_some() && rel.components().all(|c| matches!(c, Component::Normal(_)))
}

fn has_symlink_ancestor(root: &Path, rel: &Path) -> bool {
    let mut current = root.to_path_buf();
    let Some(parent) = rel.parent() else { return false };
    for component in parent.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
#[path = "working_tree_snapshot_tests.rs"]
mod tests;
