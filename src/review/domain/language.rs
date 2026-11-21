use serde::Serialize;

/// Programming language of a changed file, derived from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Go,
    Java,
    JavaScript,
    TypeScript,
    Python,
    Rust,
    Other,
}

impl Language {
    pub fn from_path(path: &str) -> Self {
        let extension = path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default();
        match extension.as_str() {
            "go" => Self::Go,
            "java" => Self::Java,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Self::TypeScript,
            "py" | "pyi" => Self::Python,
            "rs" => Self::Rust,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::Java => "java",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_languages_from_extension() {
        assert_eq!(Language::from_path("cmd/main.go"), Language::Go);
        assert_eq!(Language::from_path("src/App.java"), Language::Java);
        assert_eq!(Language::from_path("web/index.JSX"), Language::JavaScript);
        assert_eq!(Language::from_path("web/app.tsx"), Language::TypeScript);
        assert_eq!(Language::from_path("tools/x.py"), Language::Python);
        assert_eq!(Language::from_path("src/lib.rs"), Language::Rust);
    }

    #[test]
    fn unknown_or_missing_extension_is_other() {
        assert_eq!(Language::from_path("Makefile"), Language::Other);
        assert_eq!(Language::from_path("README.md"), Language::Other);
        assert_eq!(Language::Other.as_str(), "other");
    }
}
