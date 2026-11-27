use std::fs;
use std::path::{Path, PathBuf};

use crate::eval::application::{CaseRepository, EvalError, LoadedCase, NewCase};
use crate::eval::domain::case::{is_safe_relative_path, parse_case, render_case, validate_case_id};

const CASE_FILE: &str = "case.toml";
pub const BASE_DIR: &str = "base";
const PATCH_FILE: &str = "change.patch";

pub struct FsCaseRepository {
    root: PathBuf,
}

impl FsCaseRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn case_dir(&self, id: &str) -> Result<PathBuf, EvalError> {
        validate_case_id(id)?;
        Ok(self.root.join(id))
    }
}

fn storage(context: &str, path: &Path, error: impl std::fmt::Display) -> EvalError {
    EvalError::Storage(format!("{context} {}: {error}", path.display()))
}

impl CaseRepository for FsCaseRepository {
    fn ids(&self) -> Result<Vec<String>, EvalError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(storage("cannot read", &self.root, e)),
        };
        let mut ids: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()) && entry.path().join(CASE_FILE).is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| validate_case_id(name).is_ok())
            .collect();
        ids.sort();
        Ok(ids)
    }

    fn load(&self, id: &str) -> Result<LoadedCase, EvalError> {
        let dir = self.case_dir(id)?;
        let case_file = dir.join(CASE_FILE);
        let text = fs::read_to_string(&case_file).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => EvalError::NotFound(format!("case '{id}' does not exist")),
            _ => storage("cannot read", &case_file, e),
        })?;
        let case = parse_case(&text).map_err(|e| EvalError::InvalidInput(format!("{}: {e}", case_file.display())))?;
        if case.id != id {
            return Err(EvalError::InvalidInput(format!(
                "{}: id '{}' must match the folder name",
                case_file.display(),
                case.id
            )));
        }
        let base_dir = dir.join(BASE_DIR);
        if !base_dir.is_dir() {
            return Err(EvalError::InvalidInput(format!("case '{id}' has no {BASE_DIR}/ folder")));
        }
        let patch_file = dir.join(PATCH_FILE);
        let patch = fs::read_to_string(&patch_file).map_err(|e| storage("cannot read", &patch_file, e))?;
        Ok(LoadedCase { case, base_dir, patch })
    }

    fn write(&self, new: &NewCase) -> Result<(), EvalError> {
        let dir = self.case_dir(&new.case.id)?;
        if dir.exists() {
            return Err(EvalError::InvalidInput(format!(
                "case '{}' already exists in {}",
                new.case.id,
                self.root.display()
            )));
        }
        let rendered = render_case(&new.case)?;
        if let Some(bad) = new.base_files.iter().find(|f| !is_safe_relative_path(&f.path)) {
            return Err(EvalError::InvalidInput(format!("unsafe path '{}' in the base tree", bad.path)));
        }
        let result = write_case(&dir, &rendered, new);
        if result.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        result
    }
}

fn write_case(dir: &Path, rendered: &str, new: &NewCase) -> Result<(), EvalError> {
    let base = dir.join(BASE_DIR);
    fs::create_dir_all(&base).map_err(|e| storage("cannot create", &base, e))?;
    for file in &new.base_files {
        let target = base.join(&file.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| storage("cannot create", parent, e))?;
        }
        fs::write(&target, &file.bytes).map_err(|e| storage("cannot write", &target, e))?;
    }
    let patch = dir.join(PATCH_FILE);
    fs::write(&patch, &new.patch).map_err(|e| storage("cannot write", &patch, e))?;
    let case_file = dir.join(CASE_FILE);
    fs::write(&case_file, rendered).map_err(|e| storage("cannot write", &case_file, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::application::BaseFile;

    fn new_case(id: &str) -> NewCase {
        NewCase {
            case: parse_case(&format!(
                "id = \"{id}\"\nlanguage = \"go\"\n[[expected]]\nfile = \"a.go\"\nline = 1\ncategory = \"security\"\n"
            ))
            .unwrap(),
            base_files: vec![
                BaseFile { path: "a.go".into(), bytes: b"package a\n".to_vec() },
                BaseFile { path: "sub/b.go".into(), bytes: b"package b\n".to_vec() },
            ],
            patch: "diff --git a/a.go b/a.go\n".into(),
        }
    }

    #[test]
    fn a_written_case_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let repo = FsCaseRepository::new(dir.path());

        repo.write(&new_case("my-case")).unwrap();
        let loaded = repo.load("my-case").unwrap();

        assert_eq!(loaded.case.id, "my-case");
        assert_eq!(loaded.patch, "diff --git a/a.go b/a.go\n");
        assert_eq!(fs::read(loaded.base_dir.join("sub/b.go")).unwrap(), b"package b\n");
        assert_eq!(repo.ids().unwrap(), ["my-case"]);
    }

    #[test]
    fn ids_are_sorted_and_skip_folders_that_are_not_cases() {
        let dir = tempfile::tempdir().unwrap();
        let repo = FsCaseRepository::new(dir.path());
        repo.write(&new_case("b-case")).unwrap();
        repo.write(&new_case("a-case")).unwrap();
        fs::create_dir(dir.path().join("no-case-file")).unwrap();
        fs::create_dir_all(dir.path().join("Bad_Name")).unwrap();
        fs::write(dir.path().join("Bad_Name").join(CASE_FILE), "").unwrap();
        fs::write(dir.path().join("stray.txt"), "").unwrap();

        assert_eq!(repo.ids().unwrap(), ["a-case", "b-case"]);
    }

    #[test]
    fn a_missing_cases_folder_is_simply_empty() {
        let repo = FsCaseRepository::new("/definitely/not/here");

        assert!(repo.ids().unwrap().is_empty());
    }

    #[test]
    fn ids_that_could_escape_the_cases_folder_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let repo = FsCaseRepository::new(dir.path());

        for id in ["../x", "a/b", "..", "", "A"] {
            assert!(matches!(repo.load(id), Err(EvalError::Case(_))), "{id}");
        }
        let mut escaping = new_case("fine");
        escaping.case.id = "../escape".into();
        assert!(matches!(repo.write(&escaping), Err(EvalError::Case(_))));
        assert!(!dir.path().parent().unwrap().join("escape").exists());
    }

    #[test]
    fn write_refuses_to_overwrite_and_unsafe_base_paths() {
        let dir = tempfile::tempdir().unwrap();
        let repo = FsCaseRepository::new(dir.path());
        repo.write(&new_case("same")).unwrap();

        assert!(
            matches!(repo.write(&new_case("same")), Err(EvalError::InvalidInput(m)) if m.contains("already exists"))
        );
        let mut unsafe_case = new_case("unsafe");
        unsafe_case.base_files.push(BaseFile { path: "../out.txt".into(), bytes: vec![] });
        assert!(matches!(repo.write(&unsafe_case), Err(EvalError::InvalidInput(m)) if m.contains("unsafe path")));
        assert!(!dir.path().join("unsafe").exists());
    }

    #[test]
    fn load_reports_missing_and_inconsistent_cases() {
        let dir = tempfile::tempdir().unwrap();
        let repo = FsCaseRepository::new(dir.path());
        repo.write(&new_case("real")).unwrap();
        fs::rename(dir.path().join("real"), dir.path().join("renamed")).unwrap();
        repo.write(&new_case("nobase")).unwrap();
        fs::remove_dir_all(dir.path().join("nobase").join(BASE_DIR)).unwrap();

        assert!(matches!(repo.load("absent"), Err(EvalError::NotFound(_))));
        assert!(matches!(repo.load("renamed"), Err(EvalError::InvalidInput(m)) if m.contains("folder name")));
        assert!(matches!(repo.load("nobase"), Err(EvalError::InvalidInput(m)) if m.contains("base/")));
    }

    #[test]
    fn an_invalid_case_file_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("broken")).unwrap();
        fs::write(dir.path().join("broken").join(CASE_FILE), "id = \"broken\"\n").unwrap();

        let error = FsCaseRepository::new(dir.path()).load("broken").unwrap_err();

        assert!(matches!(&error, EvalError::InvalidInput(m) if m.contains("case.toml")), "{error}");
    }
}
