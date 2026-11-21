use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::access::application::{AccessError, SecretStore};
use crate::access::domain::{DashboardSecret, SECRET_BYTES};

pub const TOKEN_FILE_NAME: &str = "dashboard-token";
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join(TOKEN_FILE_NAME))
    }

    fn read(&self) -> io::Result<Result<DashboardSecret, AccessError>> {
        let content = fs::read_to_string(&self.path)?;
        restrict_permissions(&self.path)?;
        Ok(DashboardSecret::parse(&content)
            .map_err(|e| self.storage_error(format!("is invalid ({e}); delete it to generate a new dashboard token"))))
    }

    fn create(&self) -> io::Result<DashboardSecret> {
        let mut bytes = [0u8; SECRET_BYTES];
        getrandom::fill(&mut bytes).map_err(io::Error::other)?;
        let secret = DashboardSecret::from_random(&bytes);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(FILE_MODE);
        }
        let mut file = options.open(&self.path)?;
        writeln!(file, "{}", secret.expose())?;
        file.sync_all()?;
        Ok(secret)
    }

    fn storage_error(&self, what: impl std::fmt::Display) -> AccessError {
        AccessError::Storage(format!("dashboard token file {} {what}", self.path.display()))
    }
}

impl SecretStore for FileSecretStore {
    fn load_or_create(&self) -> Result<DashboardSecret, AccessError> {
        match self.read() {
            Ok(result) => return result,
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(self.storage_error(format!("unreadable: {e}"))),
            Err(_) => {}
        }
        match self.create() {
            Ok(secret) => Ok(secret),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                self.read().map_err(|e| self.storage_error(format!("unreadable: {e}")))?
            }
            Err(e) => Err(self.storage_error(format!("cannot be created: {e}"))),
        }
    }
}

fn restrict_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        if mode != FILE_MODE {
            fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, FileSecretStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::in_data_dir(&dir.path().join("nested"));
        (dir, store)
    }

    #[test]
    fn creates_a_private_file_once_and_reuses_it() {
        let (_dir, store) = store();
        let first = store.load_or_create().unwrap();
        assert_eq!(first.expose().len(), SECRET_BYTES * 2);
        assert_eq!(fs::read_to_string(&store.path).unwrap(), format!("{}\n", first.expose()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let again = FileSecretStore::new(store.path.clone()).load_or_create().unwrap();
        assert_eq!(again, first);
    }

    #[cfg(unix)]
    #[test]
    fn tightens_a_loosened_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = store();
        let secret = store.load_or_create().unwrap();
        fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(store.load_or_create().unwrap(), secret);
        assert_eq!(fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn invalid_content_is_an_error_that_names_the_file_not_the_value() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        fs::write(&store.path, "leaky-short").unwrap();
        let message = store.load_or_create().unwrap_err().to_string();
        assert!(message.contains(TOKEN_FILE_NAME) && message.contains("delete it"), "{message}");
        assert!(!message.contains("leaky-short"), "{message}");
    }

    #[test]
    fn unreadable_or_uncreatable_paths_are_storage_errors() {
        let dir = tempfile::tempdir().unwrap();
        let as_dir = FileSecretStore::new(dir.path());
        assert!(matches!(as_dir.load_or_create(), Err(AccessError::Storage(m)) if m.contains("unreadable")));
        let blocker = dir.path().join("file");
        fs::write(&blocker, "x").unwrap();
        let below_file = FileSecretStore::new(blocker.join(TOKEN_FILE_NAME));
        assert!(matches!(below_file.load_or_create(), Err(AccessError::Storage(_))));
    }

    #[test]
    fn a_racing_creator_wins_and_its_secret_is_used() {
        let (_dir, store) = store();
        let theirs = store.load_or_create().unwrap();
        let err = store.create().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(store.load_or_create().unwrap(), theirs);
    }
}
