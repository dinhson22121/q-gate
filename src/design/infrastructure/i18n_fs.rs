use std::fs;
use std::path::{Path, PathBuf};

use futures::future::BoxFuture;
use serde_json::Value;

use crate::design::application::I18nSource;
use crate::design::domain::ui::is_i18n_resource;

const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 400;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const SKIPPED_DIRS: [&str; 8] = ["node_modules", ".git", "dist", "build", "target", "vendor", ".next", "coverage"];

#[derive(Debug, Clone, Copy, Default)]
pub struct FsI18nSource;

impl I18nSource for FsI18nSource {
    fn load<'a>(&'a self, repo_root: &'a Path) -> BoxFuture<'a, Vec<String>> {
        let root = repo_root.to_path_buf();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || load_texts(&root)).await.unwrap_or_else(|e| {
                eprintln!("qgate design: i18n reader failed: {e}");
                Vec::new()
            })
        })
    }
}

pub fn load_texts(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect(root, root, 0, &mut files);
    let mut total = 0;
    let mut texts = Vec::new();
    for (path, size) in files {
        total += size;
        if total > MAX_TOTAL_BYTES {
            break;
        }
        let Ok(content) = fs::read_to_string(&path) else { continue };
        let is_json = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
        match serde_json::from_str::<Value>(&content).ok().filter(|_| is_json) {
            Some(json) => string_leaves(&json, &mut texts),
            None => texts.push(content),
        }
    }
    texts
}

fn collect(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64)>) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if meta.is_dir() && !SKIPPED_DIRS.contains(&name.as_str()) {
            collect(root, &entry.path(), depth + 1, out);
        } else if meta.is_file() && meta.len() <= MAX_FILE_BYTES && out.len() < MAX_FILES {
            let relative = entry.path().strip_prefix(root).map(|p| p.to_string_lossy().replace('\\', "/"));
            if relative.is_ok_and(|r| is_i18n_resource(&r)) {
                out.push((entry.path(), meta.len()));
            }
        }
    }
}

fn string_leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| string_leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| string_leaves(v, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_json_leaves_and_yaml_under_i18n_dirs_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("public/locales/en")).unwrap();
        fs::create_dir_all(root.join("src/i18n")).unwrap();
        fs::create_dir_all(root.join("node_modules/x/locales")).unwrap();
        fs::write(root.join("public/locales/en/common.json"), r#"{"pay": {"cta": "Pay now", "n": 1}, "l": ["A b"]}"#)
            .unwrap();
        fs::write(root.join("src/i18n/vi.yml"), "pay:\n  cta: Thanh toán\n").unwrap();
        fs::write(root.join("src/i18n/broken.json"), "{ not json").unwrap();
        fs::write(root.join("node_modules/x/locales/en.json"), r#"{"x": "vendored"}"#).unwrap();
        fs::write(root.join("config.json"), r#"{"x": "not i18n"}"#).unwrap();
        let texts = FsI18nSource.load(root).await;
        assert!(texts.contains(&"Pay now".to_string()));
        assert!(texts.contains(&"A b".to_string()));
        assert!(texts.iter().any(|t| t.contains("Thanh toán")));
        assert!(texts.iter().any(|t| t.contains("not json")), "unparsable json is kept raw");
        assert!(!texts.iter().any(|t| t.contains("vendored") || t.contains("not i18n")));
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinks() {
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(outside.path().join("locales")).unwrap();
        fs::write(outside.path().join("locales/en.json"), r#"{"x": "outside"}"#).unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("locales"), repo.path().join("locales")).unwrap();
        assert!(load_texts(repo.path()).is_empty());
        assert!(load_texts(Path::new("/nonexistent/qgate")).is_empty());
    }
}
