use super::sha256::sha256_hex;

pub const RULE_SOURCE_CAP: usize = 20 * 1024;
pub const MAX_RULE_SOURCES: usize = 40;
pub const LAYOUT_DEPTH: usize = 3;

const IGNORED_DIRS: [&str; 14] = [
    ".git",
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".gradle",
    ".idea",
    ".next",
    "coverage",
];

const RULE_FILE_NAMES: [&str; 16] = [
    "CLAUDE.md",
    "AGENTS.md",
    "CONTRIBUTING.md",
    ".golangci.yml",
    ".golangci.yaml",
    "checkstyle.xml",
    "pmd.xml",
    ".editorconfig",
    "ruff.toml",
    ".ruff.toml",
    "clippy.toml",
    ".clippy.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "pyproject.toml",
    "eslint.config.js",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub content: String,
}

pub fn is_ignored_path(path: &str) -> bool {
    path.split('/').any(|part| IGNORED_DIRS.contains(&part))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn is_rule_source(path: &str) -> bool {
    if is_ignored_path(path) {
        return false;
    }
    let name = file_name(path);
    RULE_FILE_NAMES.contains(&name)
        || name.starts_with(".eslintrc")
        || (name.starts_with("eslint.config.") && name.len() > "eslint.config.".len())
        || path == ".qgate/rules.md"
        || path.starts_with("docs/adr/")
        || path.starts_with("doc/adr/")
}

pub fn rule_source_paths(paths: &[String]) -> Vec<String> {
    let mut sources: Vec<&String> = paths.iter().filter(|p| is_rule_source(p)).collect();
    sources.sort_by_key(|p| (p.matches('/').count(), p.as_str()));
    sources.into_iter().take(MAX_RULE_SOURCES).cloned().collect()
}

pub fn relevant_content(path: &str, raw: &str) -> Option<String> {
    if file_name(path) == "pyproject.toml" {
        return ruff_sections(raw);
    }
    Some(raw.to_string())
}

fn ruff_sections(raw: &str) -> Option<String> {
    let mut keep = false;
    let mut out = String::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            let header = trimmed.trim_start_matches('[').trim_end().trim_end_matches(']').trim();
            keep = header == "tool.ruff" || header.starts_with("tool.ruff.");
        }
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

pub fn layout(paths: &[String]) -> Vec<String> {
    let mut dirs = std::collections::BTreeSet::new();
    for path in paths.iter().filter(|p| !is_ignored_path(p)) {
        let parts: Vec<&str> = path.split('/').collect();
        let dir_parts = &parts[..parts.len().saturating_sub(1)];
        for depth in 1..=dir_parts.len().min(LAYOUT_DEPTH) {
            dirs.insert(dir_parts[..depth].join("/"));
        }
    }
    dirs.into_iter().collect()
}

pub fn fingerprint(sources: &[SourceFile], layout: &[String]) -> String {
    let mut sorted: Vec<&SourceFile> = sources.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut data = Vec::new();
    for source in sorted {
        data.extend_from_slice(source.path.as_bytes());
        data.push(0);
        data.extend_from_slice(source.content.len().to_string().as_bytes());
        data.push(0);
        data.extend_from_slice(source.content.as_bytes());
        data.push(0);
    }
    data.extend_from_slice(b"\0layout\0");
    for dir in layout {
        data.extend_from_slice(dir.as_bytes());
        data.push(b'\n');
    }
    sha256_hex(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn source(path: &str, content: &str) -> SourceFile {
        SourceFile { path: path.into(), content: content.into() }
    }

    #[test]
    fn recognises_rule_sources() {
        for yes in [
            "CLAUDE.md",
            "svc/AGENTS.md",
            "docs/adr/0001-layers.md",
            ".qgate/rules.md",
            ".eslintrc.json",
            "web/eslint.config.mjs",
            "pyproject.toml",
            "rustfmt.toml",
        ] {
            assert!(is_rule_source(yes), "{yes}");
        }
        for no in ["README.md", "src/main.rs", "node_modules/x/CLAUDE.md", "eslint.config.", "docs/adr.md"] {
            assert!(!is_rule_source(no), "{no}");
        }
    }

    #[test]
    fn rule_sources_are_shallowest_first_and_bounded() {
        let mut all = paths(&["a/b/CLAUDE.md", "CLAUDE.md", "a/AGENTS.md"]);
        all.extend((0..60).map(|i| format!("docs/adr/{i:03}.md")));
        let picked = rule_source_paths(&all);
        assert_eq!(picked.len(), MAX_RULE_SOURCES);
        assert_eq!(picked[0], "CLAUDE.md");
        assert_eq!(picked[1], "a/AGENTS.md");
    }

    #[test]
    fn pyproject_keeps_only_ruff_tables() {
        let raw =
            "[project]\nname='x'\n[tool.ruff]\nline-length=100\n[tool.ruff.lint]\nselect=['E']\n[tool.black]\nx=1\n";
        assert_eq!(
            relevant_content("pyproject.toml", raw).unwrap(),
            "[tool.ruff]\nline-length=100\n[tool.ruff.lint]\nselect=['E']\n"
        );
        assert_eq!(relevant_content("pyproject.toml", "[project]\nname='x'\n"), None);
        assert_eq!(relevant_content("CLAUDE.md", "rules").as_deref(), Some("rules"));
    }

    #[test]
    fn layout_lists_dirs_up_to_depth_three() {
        let all = paths(&["src/a/b/c/d.rs", "src/main.rs", "README.md", "target/debug/x"]);
        assert_eq!(layout(&all), paths(&["src", "src/a", "src/a/b"]));
    }

    #[test]
    fn fingerprint_changes_with_sources_and_layout_only() {
        let base = fingerprint(&[source("CLAUDE.md", "a"), source("b.toml", "x")], &paths(&["src"]));
        let reordered = fingerprint(&[source("b.toml", "x"), source("CLAUDE.md", "a")], &paths(&["src"]));
        assert_eq!(base, reordered);
        assert_eq!(base.len(), 64);
        assert_ne!(base, fingerprint(&[source("CLAUDE.md", "b"), source("b.toml", "x")], &paths(&["src"])));
        assert_ne!(base, fingerprint(&[source("CLAUDE.md", "a"), source("b.toml", "x")], &paths(&["src", "lib"])));
        assert_ne!(base, fingerprint(&[source("CLAUDE.md", "a")], &paths(&["src"])));
    }
}
