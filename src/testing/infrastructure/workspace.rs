use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};

use crate::testing::application::ports::{SearchHit, Workspace};
use crate::testing::domain::paths::normalize_relative;
use crate::testing::domain::profile::ProbeFs;

const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
const MAX_HIT_CHARS: usize = 300;
const SKIPPED_DIRS: &[&str] =
    &[".git", "target", "node_modules", "build", ".gradle", "__pycache__", ".venv", ".mypy_cache", ".pytest_cache"];

#[derive(Debug, Clone)]
pub struct SnapshotWorkspace {
    root: PathBuf,
}

impl SnapshotWorkspace {
    fn resolve_existing(&self, rel: &str) -> Result<PathBuf, String> {
        let rel = normalize_relative(rel).map_err(|e| e.to_string())?;
        let canonical = self.root.join(&rel).canonicalize().map_err(|e| format!("'{rel}': {e}"))?;
        if !canonical.starts_with(&self.root) {
            return Err(format!("'{rel}' resolves outside the repository"));
        }
        Ok(canonical)
    }

    fn prepare_parent(&self, rel: &str) -> Result<PathBuf, String> {
        let mut dir = self.root.clone();
        let parts: Vec<&str> = rel.split('/').collect();
        let (file, parents) = parts.split_last().ok_or("empty path")?;
        for part in parents {
            dir.push(part);
            match fs::symlink_metadata(&dir) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(format!("'{rel}': symlinked directory")),
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(format!("'{rel}': a parent is not a directory")),
                Err(_) => fs::create_dir(&dir).map_err(|e| format!("'{rel}': {e}"))?,
            }
        }
        Ok(dir.join(file))
    }

    fn walk(&self, dir: &Path, glob: Option<&GlobMatcher>, literal: &str, max: usize, hits: &mut Vec<SearchHit>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if hits.len() >= max {
                return;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                if !SKIPPED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()) {
                    self.walk(&path, glob, literal, max, hits);
                }
            } else if kind.is_file() {
                self.search_file(&path, glob, literal, max, hits);
            }
        }
    }

    fn search_file(
        &self,
        path: &Path,
        glob: Option<&GlobMatcher>,
        literal: &str,
        max: usize,
        hits: &mut Vec<SearchHit>,
    ) {
        let Ok(rel) = path.strip_prefix(&self.root) else { return };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if glob.is_some_and(|g| !g.is_match(&rel)) {
            return;
        }
        if fs::metadata(path).map_or(true, |m| m.len() > MAX_SEARCH_FILE_BYTES) {
            return;
        }
        let Ok(text) = fs::read_to_string(path) else { return };
        for (index, line) in text.lines().enumerate() {
            if hits.len() >= max {
                return;
            }
            if line.contains(literal) {
                let text: String = line.trim().chars().take(MAX_HIT_CHARS).collect();
                hits.push(SearchHit { file: rel.clone(), line: u32::try_from(index + 1).unwrap_or(u32::MAX), text });
            }
        }
    }
}

impl ProbeFs for SnapshotWorkspace {
    fn has_path(&self, rel: &str) -> bool {
        normalize_relative(rel).is_ok_and(|rel| fs::symlink_metadata(self.root.join(rel)).is_ok())
    }

    fn entries(&self, rel: &str) -> Vec<String> {
        self.list_dir(rel).unwrap_or_default()
    }
}

impl Workspace for SnapshotWorkspace {
    fn open(root: &Path) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|e| format!("{}: {e}", root.display()))?;
        if !root.is_dir() {
            return Err(format!("{} is not a directory", root.display()));
        }
        Ok(Self { root })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn list_dir(&self, rel: &str) -> Result<Vec<String>, String> {
        let dir = self.resolve_existing(rel)?;
        let entries = fs::read_dir(&dir).map_err(|e| format!("'{rel}': {e}"))?;
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != ".git")
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.file_type().is_ok_and(|t| t.is_dir()) { format!("{name}/") } else { name }
            })
            .collect();
        names.sort();
        Ok(names)
    }

    fn read_text(&self, rel: &str) -> Result<String, String> {
        let path = self.resolve_existing(rel)?;
        let meta = fs::metadata(&path).map_err(|e| format!("'{rel}': {e}"))?;
        if !meta.is_file() {
            return Err(format!("'{rel}' is not a file"));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!("'{rel}' is larger than 5 MB"));
        }
        fs::read_to_string(&path).map_err(|e| format!("'{rel}': {e}"))
    }

    fn search(&self, literal: &str, glob: Option<&str>, max_hits: usize) -> Result<Vec<SearchHit>, String> {
        let matcher = glob
            .map(|g| Glob::new(g).map(|g| g.compile_matcher()).map_err(|e| format!("invalid glob: {e}")))
            .transpose()?;
        let mut hits = Vec::new();
        self.walk(&self.root, matcher.as_ref(), literal, max_hits, &mut hits);
        Ok(hits)
    }

    fn write_file(&self, rel: &str, content: &str, create_new: bool) -> Result<(), String> {
        let rel = normalize_relative(rel).map_err(|e| e.to_string())?;
        if rel.is_empty() {
            return Err("empty path".into());
        }
        let path = self.prepare_parent(&rel)?;
        let mut options = OpenOptions::new();
        options.write(true);
        if create_new {
            options.create_new(true);
        } else {
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() => {}
                _ => return Err(format!("'{rel}' is not the test file written earlier")),
            }
            options.truncate(true);
        }
        let mut file = no_follow(&mut options).open(&path).map_err(|e| format!("'{rel}': {e}"))?;
        if !file.metadata().is_ok_and(|meta| meta.is_file()) {
            return Err(format!("'{rel}' is not a regular file"));
        }
        file.write_all(content.as_bytes()).map_err(|e| format!("'{rel}': {e}"))
    }
}

fn no_follow(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW)
    }
    #[cfg(not(unix))]
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> (tempfile::TempDir, SnapshotWorkspace) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("calc")).unwrap();
        fs::write(dir.path().join("calc/calc.go"), "package calc\n\nfunc Add(a, b int) int { return a - b }\n")
            .unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "Add secret").unwrap();
        let workspace = SnapshotWorkspace::open(dir.path()).unwrap();
        (dir, workspace)
    }

    #[test]
    fn open_requires_an_existing_directory() {
        assert!(SnapshotWorkspace::open(Path::new("/definitely/not/here")).is_err());
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(SnapshotWorkspace::open(file.path()).unwrap_err().contains("not a directory"));
    }

    #[test]
    fn lists_reads_and_searches_inside_the_snapshot() {
        let (_dir, ws) = snapshot();
        assert_eq!(ws.list_dir(".").unwrap(), ["calc/"]);
        assert_eq!(ws.entries("calc"), ["calc.go"]);
        assert!(ws.read_text("calc/calc.go").unwrap().contains("func Add"));
        assert!(ws.read_text("calc").unwrap_err().contains("not a file"));
        let hits = ws.search("Add", None, 10).unwrap();
        assert_eq!(
            hits,
            [SearchHit {
                file: "calc/calc.go".into(),
                line: 3,
                text: "func Add(a, b int) int { return a - b }".into()
            }]
        );
        assert!(ws.search("Add", Some("**/*.py"), 10).unwrap().is_empty());
        assert_eq!(ws.search("a", None, 1).unwrap().len(), 1);
        assert!(ws.search("x", Some("[bad"), 10).unwrap_err().contains("invalid glob"));
        assert!(ws.has_path("calc/calc.go") && !ws.has_path("nope") && !ws.has_path("../x"));
        assert!(ws.list_dir("missing").is_err());
    }

    #[test]
    fn escape_attempts_are_rejected() {
        let (_dir, ws) = snapshot();
        for bad in ["../etc/passwd", "/etc/passwd", "calc/../../x"] {
            assert!(ws.read_text(bad).is_err(), "{bad}");
            assert!(ws.list_dir(bad).is_err(), "{bad}");
            assert!(ws.write_file(bad, "x", true).is_err(), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape() {
        let (dir, ws) = snapshot();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "TOKEN").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), dir.path().join("calc/link.txt")).unwrap();
        assert!(ws.read_text("out/secret.txt").unwrap_err().contains("outside"));
        assert!(ws.read_text("calc/link.txt").unwrap_err().contains("outside"));
        assert!(ws.list_dir("out").is_err());
        assert!(ws.write_file("out/new_test.go", "x", true).unwrap_err().contains("symlinked"));
        assert!(ws.write_file("calc/link.txt", "x", true).is_err(), "create_new never follows a symlink");
        assert!(ws.write_file("calc/link.txt", "x", false).is_err());
        assert!(ws.search("TOKEN", None, 10).unwrap().is_empty(), "search does not follow symlinks");
        assert_eq!(fs::read_to_string(outside.path().join("secret.txt")).unwrap(), "TOKEN");
        assert!(!outside.path().join("new_test.go").exists());
    }

    #[cfg(unix)]
    #[test]
    fn open_refuses_a_symlink_swapped_in_after_the_path_check() {
        let (dir, _ws) = snapshot();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("victim.txt");
        fs::write(&target, "PRECIOUS").unwrap();
        let swapped = dir.path().join("calc/swapped_test.go");
        std::os::unix::fs::symlink(&target, &swapped).unwrap();

        let mut options = OpenOptions::new();
        options.write(true).truncate(true);

        assert!(no_follow(&mut options).open(&swapped).is_err(), "O_NOFOLLOW rejects the final symlink");

        assert_eq!(fs::read_to_string(&target).unwrap(), "PRECIOUS");
    }

    #[cfg(unix)]
    #[test]
    fn rewrite_never_truncates_through_a_symlink() {
        let (dir, ws) = snapshot();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("victim.txt");
        fs::write(&target, "PRECIOUS").unwrap();
        ws.write_file("calc/t_test.go", "one", true).unwrap();
        fs::remove_file(dir.path().join("calc/t_test.go")).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("calc/t_test.go")).unwrap();

        assert!(ws.write_file("calc/t_test.go", "two", false).is_err());
        assert!(ws.write_file("calc/t_test.go", "two", true).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "PRECIOUS");
    }

    #[test]
    fn writes_new_files_and_rewrites_only_regular_files() {
        let (dir, ws) = snapshot();
        ws.write_file("pkg/tests/test_x.py", "one", true).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("pkg/tests/test_x.py")).unwrap(), "one");
        assert!(ws.write_file("pkg/tests/test_x.py", "two", true).is_err(), "create_new refuses existing files");
        ws.write_file("pkg/tests/test_x.py", "two", false).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("pkg/tests/test_x.py")).unwrap(), "two");
        assert!(ws.write_file("calc/calc.go/x.go", "x", true).unwrap_err().contains("not a directory"));
        assert!(ws.write_file("", "x", true).is_err());
        assert!(ws.write_file("pkg", "x", false).is_err());
    }
}
