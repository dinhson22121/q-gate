//! Single-instance guard: an exclusive lock on `<data_dir>/qgate/watch.lock`.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::watch::application::ports::WatchError;

const APP_DIR: &str = "qgate";
const LOCK_FILE: &str = "watch.lock";

/// Held for the lifetime of the watcher; the OS releases the lock when the process exits,
/// even on a crash, so a stale file never blocks a restart.
#[derive(Debug)]
pub struct InstanceGuard {
    _file: File,
    path: PathBuf,
}

impl InstanceGuard {
    /// `<data_dir>/qgate/watch.lock`, when the platform has a data directory.
    pub fn default_path() -> Option<PathBuf> {
        dirs::data_dir().map(|dir| dir.join(APP_DIR).join(LOCK_FILE))
    }

    /// Takes the lock and records the current pid, or fails with
    /// [`WatchError::AlreadyRunning`] when another process holds it.
    pub fn acquire(path: &Path) -> Result<Self, WatchError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(WatchError::AlreadyRunning { pid: read_pid(&mut file) });
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        write!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(Self { _file: file, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Pid recorded by the lock holder (unreadable on platforms with mandatory locks).
fn read_pid(file: &mut File) -> Option<u32> {
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_is_rejected_until_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(LOCK_FILE);

        let first = InstanceGuard::acquire(&path).unwrap();
        assert_eq!(first.path(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), std::process::id().to_string());

        match InstanceGuard::acquire(&path) {
            Err(WatchError::AlreadyRunning { pid }) => {
                if cfg!(unix) {
                    assert_eq!(pid, Some(std::process::id()));
                }
            }
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }

        drop(first);
        // Children forked concurrently by other tests briefly inherit the descriptor (until
        // exec closes it), which can hold the lock for a few milliseconds.
        let mut reacquired = InstanceGuard::acquire(&path);
        for _ in 0..100 {
            if reacquired.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            reacquired = InstanceGuard::acquire(&path);
        }
        assert!(reacquired.is_ok(), "lock is released when the guard is dropped");
    }

    #[test]
    fn already_running_message_mentions_pid() {
        let error = WatchError::AlreadyRunning { pid: Some(42) };
        assert_eq!(error.to_string(), "another qgate watcher is already running (pid 42)");
    }
}
