use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestLanguage {
    Go,
    Java,
    Python,
    Rust,
}

impl TestLanguage {
    pub const ALL: [Self; 4] = [Self::Go, Self::Java, Self::Python, Self::Rust];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::Java => "java",
            Self::Python => "python",
            Self::Rust => "rust",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == name.trim().to_ascii_lowercase())
    }

    pub fn toolchain(self) -> &'static str {
        self.as_str()
    }

    pub fn of_path(path: &str) -> Option<Self> {
        let extension = path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase())?;
        match extension.as_str() {
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            "py" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            _ => None,
        }
    }
}

impl fmt::Display for TestLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageChoice {
    pub language: TestLanguage,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    NoBackendFiles,
    NotEnabled(TestLanguage),
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBackendFiles => f.write_str("no changed Go, Java, Python or Rust file to test"),
            Self::NotEnabled(language) => write!(f, "language not enabled: {language}"),
        }
    }
}

pub fn select_language(changed: &[String], enabled: &[TestLanguage]) -> Result<LanguageChoice, SelectionError> {
    let relevant: Vec<(TestLanguage, &String)> = changed
        .iter()
        .filter(|path| !is_vendored(path))
        .filter_map(|path| TestLanguage::of_path(path).map(|language| (language, path)))
        .collect();
    let count = |language: TestLanguage| relevant.iter().filter(|(l, _)| *l == language).count();
    let language = TestLanguage::ALL
        .into_iter()
        .filter(|language| count(*language) > 0)
        .reduce(|best, next| if count(next) > count(best) { next } else { best })
        .ok_or(SelectionError::NoBackendFiles)?;
    if !enabled.contains(&language) {
        return Err(SelectionError::NotEnabled(language));
    }
    let files = relevant.into_iter().filter(|(l, _)| *l == language).map(|(_, path)| path.clone()).collect();
    Ok(LanguageChoice { language, files })
}

fn is_vendored(path: &str) -> bool {
    path.split('/').any(|part| matches!(part, "vendor" | "node_modules" | "third_party"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_and_names_languages() {
        for language in TestLanguage::ALL {
            assert_eq!(TestLanguage::parse(language.as_str()), Some(language));
            assert_eq!(language.to_string(), language.toolchain());
        }
        assert_eq!(TestLanguage::parse(" GO "), Some(TestLanguage::Go));
        assert_eq!(TestLanguage::parse("node"), None);
    }

    #[test]
    fn language_of_path_by_extension() {
        assert_eq!(TestLanguage::of_path("a/b.go"), Some(TestLanguage::Go));
        assert_eq!(TestLanguage::of_path("A.JAVA"), Some(TestLanguage::Java));
        assert_eq!(TestLanguage::of_path("x.py"), Some(TestLanguage::Python));
        assert_eq!(TestLanguage::of_path("src/lib.rs"), Some(TestLanguage::Rust));
        assert_eq!(TestLanguage::of_path("web/app.ts"), None);
        assert_eq!(TestLanguage::of_path("Makefile"), None);
    }

    #[test]
    fn majority_language_wins() {
        let changed = paths(&["a.go", "b.py", "c.py", "README.md", "web/x.ts"]);
        let choice = select_language(&changed, &TestLanguage::ALL).unwrap();
        assert_eq!(choice.language, TestLanguage::Python);
        assert_eq!(choice.files, paths(&["b.py", "c.py"]));
    }

    #[test]
    fn ties_follow_the_fixed_order_and_vendor_is_ignored() {
        let changed = paths(&["x.rs", "y.go", "vendor/a.rs", "vendor/b.rs"]);
        assert_eq!(select_language(&changed, &TestLanguage::ALL).unwrap().language, TestLanguage::Go);
    }

    #[test]
    fn disabled_or_missing_language_is_an_error() {
        let changed = paths(&["a.java"]);
        let error = select_language(&changed, &[TestLanguage::Go]).unwrap_err();
        assert_eq!(error, SelectionError::NotEnabled(TestLanguage::Java));
        assert_eq!(error.to_string(), "language not enabled: java");
        let error = select_language(&paths(&["web/a.tsx"]), &TestLanguage::ALL).unwrap_err();
        assert_eq!(error, SelectionError::NoBackendFiles);
        assert!(error.to_string().contains("no changed"));
    }
}
