use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::settings::application::credentials::{CredentialError, CredentialStore};
use crate::settings::domain::credentials::{CredentialName, Secret};

#[cfg(unix)]
const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

type Entries = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_entries(&self) -> Result<Entries, CredentialError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Entries::new()),
            Err(e) => return Err(self.io_error("read", &e)),
        };
        toml::from_str(&text)
            .map_err(|_| CredentialError::Store(format!("credentials file {} is malformed", self.path.display())))
    }

    fn write_entries(&self, entries: &Entries) -> Result<(), CredentialError> {
        let text =
            toml::to_string(entries).map_err(|_| CredentialError::Store("cannot serialize credentials".into()))?;
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        create_private_dir(dir).map_err(|e| self.io_error("create the directory of", &e))?;
        let temp = self.temp_path();
        let result = write_private(&temp, text.as_bytes()).and_then(|()| fs::rename(&temp, &self.path));
        if let Err(e) = result {
            let _ = fs::remove_file(&temp);
            return Err(self.io_error("write", &e));
        }
        Ok(())
    }

    fn temp_path(&self) -> PathBuf {
        let file_name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()))
    }

    fn io_error(&self, action: &str, error: &io::Error) -> CredentialError {
        CredentialError::Store(format!("cannot {action} {}: {error}", self.path.display()))
    }
}

impl CredentialStore for FileStore {
    fn get(&self, name: CredentialName) -> Result<Option<Secret>, CredentialError> {
        Ok(self.read_entries()?.get(name.as_str()).map(Secret::new))
    }

    fn set(&self, name: CredentialName, secret: &Secret) -> Result<(), CredentialError> {
        let mut entries = self.read_entries()?;
        entries.insert(name.as_str().to_string(), secret.expose().to_string());
        self.write_entries(&entries)
    }

    fn delete(&self, name: CredentialName) -> Result<(), CredentialError> {
        let mut entries = self.read_entries()?;
        if entries.remove(name.as_str()).is_none() {
            return Ok(());
        }
        self.write_entries(&entries)
    }
}

fn write_private(path: &Path, content: &[u8]) -> io::Result<()> {
    let _ = fs::remove_file(path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE);
    }
    let mut file = options.open(path)?;
    file.write_all(content)?;
    file.sync_all()
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(DIR_MODE);
    }
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: CredentialName = CredentialName::FigmaToken;
    const TOKEN: CredentialName = CredentialName::JiraApiToken;

    fn store() -> (tempfile::TempDir, FileStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("qgate").join("credentials.toml"));
        (dir, store)
    }

    #[test]
    fn missing_file_has_no_secrets() {
        let (_dir, store) = store();
        assert!(store.get(KEY).unwrap().is_none());
        store.delete(KEY).unwrap();
        assert!(!store.path().exists());
    }

    #[test]
    fn set_get_delete_round_trip() {
        let (_dir, store) = store();
        store.set(KEY, &Secret::new("sk-ant-1")).unwrap();
        store.set(TOKEN, &Secret::new("tok\"en")).unwrap();
        store.set(KEY, &Secret::new("sk-ant-2")).unwrap();
        assert_eq!(store.get(KEY).unwrap().unwrap().expose(), "sk-ant-2");
        assert_eq!(store.get(TOKEN).unwrap().unwrap().expose(), "tok\"en");
        store.delete(KEY).unwrap();
        assert!(store.get(KEY).unwrap().is_none());
        assert!(store.get(TOKEN).unwrap().is_some());
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let (_dir, store) = store();
        store.set(KEY, &Secret::new("v")).unwrap();
        let names: Vec<_> =
            fs::read_dir(store.path().parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("credentials.toml")]);
    }

    #[cfg(unix)]
    #[test]
    fn file_and_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = store();
        store.set(KEY, &Secret::new("v")).unwrap();
        let file_mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(store.path().parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn malformed_file_error_does_not_leak_content() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), "jira_api_token = sk-ant-secret-unquoted\n").unwrap();
        let message = store.get(KEY).unwrap_err().to_string();
        assert!(message.contains("malformed"));
        assert!(!message.contains("sk-ant-secret"), "{message}");
    }

    #[test]
    fn unreadable_path_is_a_store_error() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path()).unwrap();
        assert!(matches!(store.get(KEY), Err(CredentialError::Store(_))));
    }

    #[test]
    fn write_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();
        let store = FileStore::new(blocker.join("credentials.toml"));
        assert!(matches!(store.set(KEY, &Secret::new("v")), Err(CredentialError::Store(_))));
    }
}
