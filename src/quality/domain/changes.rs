use std::collections::{BTreeMap, HashMap};

use super::finding::{DependencyRef, QualityCategory, QualityFinding};

const MIN_VERSION_NEEDLE: usize = 3;
const GO_MANIFEST: &str = "go.mod";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub is_new_file: bool,
    pub added: BTreeMap<u32, String>,
}

impl ChangedFile {
    pub fn new(path: impl Into<String>, is_new_file: bool, added: BTreeMap<u32, String>) -> Self {
        Self { path: path.into(), is_new_file, added }
    }
}

#[derive(Debug, Default)]
pub struct ChangeIndex {
    files: HashMap<String, ChangedFile>,
}

impl ChangeIndex {
    pub fn new(files: &[ChangedFile]) -> Self {
        let files = files.iter().map(|f| (f.path.trim_start_matches("./").to_string(), f.clone())).collect();
        Self { files }
    }

    pub fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn is_added(&self, path: &str, line: u32) -> bool {
        self.files.get(path).is_some_and(|f| line > 0 && (f.is_new_file || f.added.contains_key(&line)))
    }

    pub fn dependency_is_new(&self, dependency: &DependencyRef) -> bool {
        let candidates: Vec<&ChangedFile> = match &dependency.manifest {
            Some(path) => self.files.get(path).into_iter().collect(),
            None => self.files.values().filter(|f| file_name(&f.path) == GO_MANIFEST).collect(),
        };
        let needles = needles(dependency);
        candidates.iter().any(|file| file.is_new_file || file.added.values().any(|line| mentions(line, &needles)))
    }

    pub fn classify(&self, finding: &mut QualityFinding) {
        let at_added_line = self.is_added(&finding.file, finding.line);
        finding.is_new = match finding.category {
            QualityCategory::Secret | QualityCategory::Architecture => at_added_line,
            QualityCategory::DependencyVuln => {
                at_added_line || finding.dependency.as_ref().is_some_and(|d| self.dependency_is_new(d))
            }
        };
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn needles(dependency: &DependencyRef) -> Vec<String> {
    let mut needles = vec![dependency.name.clone()];
    if let Some((_, artifact)) = dependency.name.split_once(':') {
        needles.push(artifact.to_string());
    }
    let version = dependency.version.trim_start_matches('v');
    if version.len() >= MIN_VERSION_NEEDLE && version.chars().any(|c| c.is_ascii_digit()) {
        needles.push(version.to_string());
    }
    needles
}

fn mentions(line: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| line.contains(needle.as_str()))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn changed(path: &str, lines: &[(u32, &str)]) -> ChangedFile {
        ChangedFile::new(path, false, lines.iter().map(|(n, t)| (*n, t.to_string())).collect())
    }

    pub fn added_file(path: &str) -> ChangedFile {
        ChangedFile::new(path, true, BTreeMap::new())
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{added_file, changed};
    use super::*;
    use crate::quality::domain::finding::QualitySeverity;
    use crate::quality::domain::finding::fixtures::finding;

    fn dep(name: &str, version: &str, manifest: Option<&str>) -> DependencyRef {
        DependencyRef {
            name: name.into(),
            version: version.into(),
            manifest: manifest.map(String::from),
            fixed_version: None,
        }
    }

    #[test]
    fn secret_is_new_only_on_added_lines() {
        let index = ChangeIndex::new(&[changed("a.go", &[(8, "token")]), added_file("b.go")]);
        let mut f = finding(QualityCategory::Secret, QualitySeverity::Critical, false);
        f.file = "a.go".into();
        f.line = 8;
        index.classify(&mut f);
        assert!(f.is_new);
        f.line = 9;
        index.classify(&mut f);
        assert!(!f.is_new, "unchanged line of a changed file is pre-existing");
        f.file = "b.go".into();
        f.line = 40;
        index.classify(&mut f);
        assert!(f.is_new, "every line of a new file is new");
        f.file = "other.go".into();
        index.classify(&mut f);
        assert!(!f.is_new);
        f.file = "b.go".into();
        f.line = 0;
        index.classify(&mut f);
        assert!(!f.is_new, "line 0 is never an added line");
    }

    #[test]
    fn dependency_is_new_when_its_manifest_entry_was_added() {
        let index = ChangeIndex::new(&[changed("go.mod", &[(5, "\tgolang.org/x/text v0.3.7")])]);
        assert!(index.dependency_is_new(&dep("golang.org/x/text", "0.3.7", Some("go.mod"))));
        assert!(!index.dependency_is_new(&dep("golang.org/x/net", "0.1.0", Some("go.mod"))));
        assert!(!index.dependency_is_new(&dep("golang.org/x/text", "0.3.7", Some("sub/go.mod"))));
    }

    #[test]
    fn version_only_bump_in_pom_counts_as_new() {
        let index = ChangeIndex::new(&[changed("pom.xml", &[(21, "<version>2.14.0</version>")])]);
        assert!(index.dependency_is_new(&dep(
            "com.fasterxml.jackson.core:jackson-databind",
            "2.14.0",
            Some("pom.xml")
        )));
        assert!(!index.dependency_is_new(&dep("org.x:y", "9", Some("pom.xml"))), "short versions are not needles");
    }

    #[test]
    fn unknown_manifest_falls_back_to_any_changed_go_mod() {
        let index = ChangeIndex::new(&[
            changed("svc/go.mod", &[(3, "require golang.org/x/text v0.3.7")]),
            changed("README.md", &[(1, "x")]),
        ]);
        assert!(index.dependency_is_new(&dep("golang.org/x/text", "v0.3.7", None)));
        assert!(!ChangeIndex::new(&[changed("README.md", &[(1, "golang.org/x/text")])]).dependency_is_new(&dep(
            "golang.org/x/text",
            "v0.3.7",
            None
        )));
    }

    #[test]
    fn brand_new_manifest_makes_every_dependency_new() {
        let index = ChangeIndex::new(&[added_file("go.mod")]);
        assert!(index.dependency_is_new(&dep("anything", "1.0.0", Some("go.mod"))));
    }

    #[test]
    fn vulnerability_called_from_an_added_line_is_new_even_if_the_manifest_is_untouched() {
        let index = ChangeIndex::new(&[changed("main.go", &[(12, "language.Parse(x)")])]);
        let mut f = finding(QualityCategory::DependencyVuln, QualitySeverity::High, false);
        f.file = "main.go".into();
        f.line = 12;
        f.dependency = Some(dep("golang.org/x/text", "v0.3.7", Some("go.mod")));
        index.classify(&mut f);
        assert!(f.is_new);
        f.line = 13;
        index.classify(&mut f);
        assert!(!f.is_new);
    }

    #[test]
    fn index_exposes_paths() {
        let index = ChangeIndex::new(&[changed("./a.go", &[]), added_file("b/c.go")]);
        let mut paths: Vec<_> = index.paths().collect();
        paths.sort_unstable();
        assert_eq!(paths, ["a.go", "b/c.go"]);
        assert!(index.contains("a.go"));
    }
}
