//! Review files on the local filesystem, confined to one root folder.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use crate::handoff::application::{ReviewFileSink, SinkError, StoredFile};

#[cfg(unix)]
const FILE_MODE: u32 = 0o644;
#[cfg(unix)]
const DIR_MODE: u32 = 0o755;
const TEMP_PREFIX: &str = ".qgate-";
const TEMP_SUFFIX: &str = ".tmp";

/// Writes inside `root` only: relative paths with plain components, no symlinks anywhere below the root,
/// atomic replace (temp file + rename), files 0644 and folders 0755.
#[derive(Debug, Clone)]
pub struct FsReviewFileSink {
    root: PathBuf,
}

impl FsReviewFileSink {
    /// Creates `root` when missing. The root itself may be reached through a symlink (e.g. a mount).
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        if !root.exists() {
            create_dirs(root)?;
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a folder", root.display())));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validated components of a relative path (empty for the root itself).
    fn components(rel: &str) -> Result<Vec<&str>, SinkError> {
        let reject = || SinkError(format!("refusing path '{rel}': must be relative, '/'-separated, without '..'"));
        if rel.is_empty() {
            return Ok(Vec::new());
        }
        if rel.contains(['\\', '\0', ':']) || Path::new(rel).is_absolute() {
            return Err(reject());
        }
        let parts: Vec<&str> = rel.split('/').collect();
        let plain =
            |part: &&str| matches!(Path::new(part).components().collect::<Vec<_>>().as_slice(), [Component::Normal(_)]);
        if parts.iter().all(plain) && parts.iter().all(|p| *p != "." && *p != "..") { Ok(parts) } else { Err(reject()) }
    }

    /// Walks `dirs` below the root, refusing symlinks and non-folders. `None` when a folder is missing and
    /// `create` is false.
    fn folder(&self, dirs: &[&str], create: bool) -> Result<Option<PathBuf>, SinkError> {
        let mut path = self.root.clone();
        for dir in dirs {
            path.push(dir);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(SinkError(format!("refusing symlinked folder {}", path.display())));
                }
                Ok(meta) if !meta.is_dir() => return Err(SinkError(format!("{} is not a folder", path.display()))),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
                    create_dirs(&path).map_err(|e| SinkError(format!("cannot create {}: {e}", path.display())))?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(SinkError(format!("cannot read {}: {e}", path.display()))),
            }
        }
        let resolved = path.canonicalize().map_err(|e| SinkError(format!("cannot resolve {}: {e}", path.display())))?;
        if !resolved.starts_with(&self.root) {
            return Err(SinkError(format!("refusing {}: outside {}", path.display(), self.root.display())));
        }
        Ok(Some(resolved))
    }

    /// Folder and file name of `rel`, refusing an existing target that is not a regular file.
    fn target(&self, rel: &str, create: bool) -> Result<Option<(PathBuf, PathBuf)>, SinkError> {
        let parts = Self::components(rel)?;
        let Some((name, dirs)) = parts.split_last() else {
            return Err(SinkError("refusing an empty file path".into()));
        };
        let Some(folder) = self.folder(dirs, create)? else { return Ok(None) };
        let file = folder.join(name);
        match fs::symlink_metadata(&file) {
            Ok(meta) if !meta.file_type().is_file() => {
                Err(SinkError(format!("refusing {}: not a regular file", file.display())))
            }
            Ok(_) => Ok(Some((folder, file))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Some((folder, file))),
            Err(e) => Err(SinkError(format!("cannot read {}: {e}", file.display()))),
        }
    }
}

impl ReviewFileSink for FsReviewFileSink {
    fn write(&self, rel_path: &str, bytes: &[u8]) -> Result<(), SinkError> {
        let (folder, file) = self.target(rel_path, true)?.ok_or_else(|| SinkError("folder missing".into()))?;
        let fail = |e: io::Error| SinkError(format!("cannot write {}: {e}", file.display()));
        let mut temp =
            tempfile::Builder::new().prefix(TEMP_PREFIX).suffix(TEMP_SUFFIX).tempfile_in(&folder).map_err(fail)?;
        temp.write_all(bytes).map_err(fail)?;
        set_mode(temp.path(), false).map_err(fail)?;
        temp.as_file().sync_all().map_err(fail)?;
        temp.persist(&file).map_err(|e| fail(e.error))?;
        Ok(())
    }

    fn read(&self, rel_path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, SinkError> {
        let Some((_, file)) = self.target(rel_path, false)? else { return Ok(None) };
        let fail = |e: io::Error| SinkError(format!("cannot read {}: {e}", file.display()));
        let mut options = fs::OpenOptions::new();
        options.read(true);
        // The target was checked to be a regular file; refuse a symlink swapped in since then.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
        let opened = match options.open(&file) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other.map_err(fail)?,
        };
        if !opened.metadata().map_err(fail)?.is_file() {
            return Err(SinkError(format!("refusing {}: not a regular file", file.display())));
        }
        let mut bytes = Vec::new();
        opened.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes).map_err(fail)?;
        if bytes.len() as u64 > max_bytes {
            return Err(SinkError(format!("refusing {}: larger than {max_bytes} bytes", file.display())));
        }
        Ok(Some(bytes))
    }

    fn list(&self, dir_prefix: &str) -> Result<Vec<StoredFile>, SinkError> {
        let dirs = Self::components(dir_prefix)?;
        let Some(folder) = self.folder(&dirs, false)? else { return Ok(Vec::new()) };
        let fail = |e: io::Error| SinkError(format!("cannot list {}: {e}", folder.display()));
        let mut files = Vec::new();
        for entry in fs::read_dir(&folder).map_err(fail)? {
            let entry = entry.map_err(fail)?;
            // `file_type` does not follow symlinks: links are never listed, so never pruned.
            if !entry.file_type().map_err(fail)?.is_file() {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else { continue };
            let modified = entry.metadata().and_then(|m| m.modified()).map_err(fail)?;
            files.push(StoredFile { name, modified });
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(files)
    }

    fn delete(&self, rel_path: &str) -> Result<(), SinkError> {
        let Some((_, file)) = self.target(rel_path, false)? else { return Ok(()) };
        match fs::remove_file(&file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(SinkError(format!("cannot delete {}: {e}", file.display())))
            }
            _ => Ok(()),
        }
    }
}

fn create_dirs(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, DIR_MODE);
    builder.create(path)?;
    set_mode(path, true)
}

/// 0755 for folders, 0644 for files (independent of the umask); no-op on other platforms.
fn set_mode(path: &Path, is_dir: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if is_dir { DIR_MODE } else { FILE_MODE };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, is_dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> (tempfile::TempDir, FsReviewFileSink) {
        let dir = tempfile::tempdir().unwrap();
        let sink = FsReviewFileSink::new(dir.path().join("reviews")).unwrap();
        (dir, sink)
    }

    #[test]
    fn writes_atomically_creating_folders_and_replacing_files() {
        let (_dir, sink) = sink();
        sink.write("shop-api/main/abc1234.md", b"one").unwrap();
        sink.write("shop-api/main/abc1234.md", b"two").unwrap();
        let file = sink.root().join("shop-api/main/abc1234.md");
        assert_eq!(fs::read_to_string(&file).unwrap(), "two");
        let names: Vec<String> = sink.list("shop-api/main").unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["abc1234.md".to_string()], "no temp files left behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o644);
            let folder = sink.root().join("shop-api/main");
            assert_eq!(fs::metadata(folder).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn rejects_paths_that_escape_the_root() {
        let (_dir, sink) = sink();
        for bad in ["../x.md", "a/../../x.md", "/etc/x.md", "a//b.md", "./a.md", "a\\b.md", "C:x.md", "a/./b.md", ""] {
            assert!(sink.write(bad, b"x").is_err(), "{bad}");
        }
        assert!(sink.delete("../x.md").is_err());
        assert!(sink.list("..").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_folders_and_files() {
        let (dir, sink) = sink();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("victim.md"), "keep").unwrap();
        std::os::unix::fs::symlink(&outside, sink.root().join("link")).unwrap();
        assert!(sink.write("link/x.md", b"x").is_err());
        assert!(sink.delete("link/victim.md").is_err());
        assert!(sink.list("link").is_err());
        std::os::unix::fs::symlink(outside.join("victim.md"), sink.root().join("file.md")).unwrap();
        assert!(sink.write("file.md", b"x").is_err());
        assert!(sink.delete("file.md").is_err());
        assert!(sink.list("").unwrap().is_empty(), "symlinks are not listed");
        assert_eq!(fs::read_to_string(outside.join("victim.md")).unwrap(), "keep");
        assert!(!outside.join("x.md").exists());
    }

    #[test]
    fn reads_regular_files_within_a_size_limit() {
        let (_dir, sink) = sink();
        assert_eq!(sink.read("a/missing.json", 10).unwrap(), None);
        assert_eq!(sink.read("missing/x.json", 10).unwrap(), None);
        sink.write("a/x.json", b"12345").unwrap();
        assert_eq!(sink.read("a/x.json", 5).unwrap(), Some(b"12345".to_vec()));
        assert!(sink.read("a/x.json", 4).unwrap_err().0.contains("larger than 4 bytes"));
        assert!(sink.read("../x.json", 10).is_err());
        fs::create_dir(sink.root().join("a/sub")).unwrap();
        assert!(sink.read("a/sub", 10).is_err(), "folders are not files");
    }

    #[cfg(unix)]
    #[test]
    fn read_refuses_symlinks() {
        let (dir, sink) = sink();
        fs::write(dir.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), sink.root().join("x.json")).unwrap();
        assert!(sink.read("x.json", 10).is_err());
    }

    #[test]
    fn delete_and_list_tolerate_missing_paths() {
        let (_dir, sink) = sink();
        assert!(sink.list("nope/deeper").unwrap().is_empty());
        sink.delete("nope/x.md").unwrap();
        sink.write("a/x.md", b"x").unwrap();
        sink.delete("a/missing.md").unwrap();
        sink.delete("a/x.md").unwrap();
        assert!(sink.list("a").unwrap().is_empty());
        fs::create_dir(sink.root().join("a/sub")).unwrap();
        assert!(sink.delete("a/sub").is_err(), "folders are never deleted");
        assert!(sink.write("a/sub/../y.md", b"x").is_err());
    }

    #[test]
    fn root_must_be_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        assert!(FsReviewFileSink::new(&file).is_err());
        fs::write(dir.path().join("blocker"), "x").unwrap();
        let sink = FsReviewFileSink::new(dir.path()).unwrap();
        assert!(sink.write("blocker/x.md", b"x").is_err(), "a file where a folder is expected");
    }
}
