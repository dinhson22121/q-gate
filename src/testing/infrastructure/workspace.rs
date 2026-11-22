use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
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
    dir: Arc<Dir>,
}

fn dir_path(rel: &str) -> &str {
    if rel.is_empty() { "." } else { rel }
}

fn io_error(rel: &str, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::PermissionDenied && error.to_string().contains("outside") {
        format!("'{rel}' resolves outside the repository")
    } else {
        format!("'{rel}': {error}")
    }
}

fn join_rel(prefix: &str, name: &str) -> String {
    if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") }
}

impl SnapshotWorkspace {
    fn prepare_parent(&self, rel: &str) -> Result<(Dir, String), String> {
        let parts: Vec<&str> = rel.split('/').collect();
        let (file, parents) = parts.split_last().ok_or("empty path")?;
        let mut dir = self.dir.try_clone().map_err(|e| io_error(rel, &e))?;
        for part in parents {
            match dir.symlink_metadata(part) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(format!("'{rel}': symlinked directory")),
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(format!("'{rel}': a parent is not a directory")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    dir.create_dir(part).map_err(|e| io_error(rel, &e))?;
                }
                Err(e) => return Err(io_error(rel, &e)),
            }
            dir = dir.open_dir(part).map_err(|e| io_error(rel, &e))?;
        }
        Ok((dir, (*file).to_string()))
    }

    fn walk(
        &self,
        dir: &Dir,
        prefix: &str,
        glob: Option<&GlobMatcher>,
        literal: &str,
        hits: &mut Vec<SearchHit>,
        max: usize,
    ) {
        let Ok(entries) = dir.entries() else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if hits.len() >= max {
                return;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = join_rel(prefix, &name);
            if kind.is_dir() {
                if SKIPPED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                if let Ok(child) = dir.open_dir(&name) {
                    self.walk(&child, &rel, glob, literal, hits, max);
                }
            } else if kind.is_file() && glob.is_none_or(|g| g.is_match(&rel)) {
                Self::search_file(dir, &name, &rel, literal, hits, max);
            }
        }
    }

    fn search_file(dir: &Dir, name: &str, rel: &str, literal: &str, hits: &mut Vec<SearchHit>, max: usize) {
        let Ok(mut file) = dir.open(name) else { return };
        if file.metadata().map_or(true, |m| m.len() > MAX_SEARCH_FILE_BYTES) {
            return;
        }
        let mut text = String::new();
        if file.read_to_string(&mut text).is_err() {
            return;
        }
        for (index, line) in text.lines().enumerate() {
            if hits.len() >= max {
                return;
            }
            if line.contains(literal) {
                let text: String = line.trim().chars().take(MAX_HIT_CHARS).collect();
                hits.push(SearchHit {
                    file: rel.to_string(),
                    line: u32::try_from(index + 1).unwrap_or(u32::MAX),
                    text,
                });
            }
        }
    }
}

impl ProbeFs for SnapshotWorkspace {
    fn has_path(&self, rel: &str) -> bool {
        normalize_relative(rel).is_ok_and(|rel| self.dir.symlink_metadata(dir_path(&rel)).is_ok())
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
        let dir = Dir::open_ambient_dir(&root, ambient_authority()).map_err(|e| format!("{}: {e}", root.display()))?;
        Ok(Self { root, dir: Arc::new(dir) })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn list_dir(&self, rel: &str) -> Result<Vec<String>, String> {
        let rel = normalize_relative(rel).map_err(|e| e.to_string())?;
        let entries = self.dir.read_dir(dir_path(&rel)).map_err(|e| io_error(&rel, &e))?;
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
        let rel = normalize_relative(rel).map_err(|e| e.to_string())?;
        let mut file = self.dir.open(dir_path(&rel)).map_err(|e| io_error(&rel, &e))?;
        let meta = file.metadata().map_err(|e| io_error(&rel, &e))?;
        if !meta.is_file() {
            return Err(format!("'{rel}' is not a file"));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!("'{rel}' is larger than 5 MB"));
        }
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(|e| io_error(&rel, &e))?;
        Ok(text)
    }

    fn search(&self, literal: &str, glob: Option<&str>, max_hits: usize) -> Result<Vec<SearchHit>, String> {
        let matcher = glob
            .map(|g| Glob::new(g).map(|g| g.compile_matcher()).map_err(|e| format!("invalid glob: {e}")))
            .transpose()?;
        let mut hits = Vec::new();
        self.walk(&self.dir, "", matcher.as_ref(), literal, &mut hits, max_hits);
        Ok(hits)
    }

    fn write_file(&self, rel: &str, content: &str, create_new: bool) -> Result<(), String> {
        let rel = normalize_relative(rel).map_err(|e| e.to_string())?;
        if rel.is_empty() {
            return Err("empty path".into());
        }
        let (parent, name) = self.prepare_parent(&rel)?;
        let mut options = OpenOptions::new();
        options.write(true);
        if create_new {
            options.create_new(true);
        } else {
            match parent.symlink_metadata(&name) {
                Ok(meta) if meta.is_file() => {}
                _ => return Err(format!("'{rel}' is not the test file written earlier")),
            }
            options.truncate(true);
        }
        let mut file = parent.open_with(&name, no_follow(&mut options)).map_err(|e| io_error(&rel, &e))?;
        if !file.metadata().is_ok_and(|meta| meta.is_file()) {
            return Err(format!("'{rel}' is not a regular file"));
        }
        file.write_all(content.as_bytes()).map_err(|e| io_error(&rel, &e))
    }
}

fn no_follow(options: &mut OpenOptions) -> &OpenOptions {
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
}

#[cfg(test)]
mod tests {
    use std::fs;

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
        let root = Dir::open_ambient_dir(dir.path(), ambient_authority()).unwrap();

        assert!(
            root.open_with("calc/swapped_test.go", no_follow(&mut options)).is_err(),
            "O_NOFOLLOW rejects the final symlink"
        );

        assert_eq!(fs::read_to_string(&target).unwrap(), "PRECIOUS");
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_symlinked_directories_never_redirect_a_write() {
        let (dir, ws) = snapshot();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(outside.path().join("deep")).unwrap();
        fs::write(outside.path().join("deep/victim.txt"), "PRECIOUS").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("calc/hop")).unwrap();
        std::os::unix::fs::symlink("../..", dir.path().join("calc/up")).unwrap();

        for rel in ["calc/hop/deep/new_test.go", "calc/hop/deep/victim.txt", "calc/up/x/new_test.go"] {
            for create_new in [true, false] {
                assert!(ws.write_file(rel, "pwned", create_new).is_err(), "{rel} {create_new}");
            }
        }
        assert!(ws.read_text("calc/hop/deep/victim.txt").unwrap_err().contains("outside"));
        assert!(ws.list_dir("calc/hop").unwrap_err().contains("outside"));
        assert_eq!(fs::read_to_string(outside.path().join("deep/victim.txt")).unwrap(), "PRECIOUS");
        assert!(!outside.path().join("deep/new_test.go").exists());
        assert!(!dir.path().join("../x").exists());
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_symlink_to_a_directory_inside_the_snapshot_is_still_refused_for_writes() {
        let (dir, ws) = snapshot();
        std::os::unix::fs::symlink("calc", dir.path().join("alias")).unwrap();
        assert!(ws.write_file("alias/new_test.go", "x", true).unwrap_err().contains("symlinked"));
        assert!(!dir.path().join("calc/new_test.go").exists());
        assert!(ws.read_text("alias/calc.go").unwrap().contains("func Add"));
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
