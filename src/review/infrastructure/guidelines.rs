use std::path::{Component, Path};

const TRUNCATED_NOTE: &str = "[truncated]";
const SECTION_SEPARATOR: &str = "\n\n";

pub async fn load_guidelines(root: &Path, files: &[String], extra: &str, max_bytes_per_file: usize) -> String {
    let mut sections = Vec::new();
    for file in files.iter().filter(|f| is_safe_relative(f)) {
        if let Some(content) = read_guideline(root, file).await {
            sections.push(section(file, &content, max_bytes_per_file));
        }
    }
    let extra = extra.trim();
    if !extra.is_empty() {
        sections.push(extra.to_string());
    }
    sections.join(SECTION_SEPARATOR)
}

fn is_safe_relative(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

async fn read_guideline(root: &Path, file: &str) -> Option<String> {
    let path = root.join(file);
    let metadata = tokio::fs::symlink_metadata(&path).await.ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let real_root = tokio::fs::canonicalize(root).await.ok()?;
    if !tokio::fs::canonicalize(&path).await.ok()?.starts_with(real_root) {
        return None;
    }
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    let content = content.trim();
    (!content.is_empty()).then(|| content.to_string())
}

fn section(path: &str, content: &str, max_bytes: usize) -> String {
    let kept = truncate_on_char_boundary(content, max_bytes);
    if kept.len() < content.len() {
        format!("## {path}\n\n{kept}\n{TRUNCATED_NOTE}")
    } else {
        format!("## {path}\n\n{content}")
    }
}

fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const LIMIT: usize = 1_000;

    fn repo(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        for (path, content) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().expect("has parent")).expect("create dirs");
            std::fs::write(full, content).expect("write");
        }
        dir
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn concatenates_existing_files_with_headings_and_extra() {
        let dir = repo(&[("CLAUDE.md", "Use tabs.\n"), ("docs/CONTRIBUTING.md", "Write tests.")]);
        let files = names(&["CLAUDE.md", "MISSING.md", "docs/CONTRIBUTING.md"]);

        let text = load_guidelines(dir.path(), &files, "  Be strict.  ", LIMIT).await;

        assert_eq!(text, "## CLAUDE.md\n\nUse tabs.\n\n## docs/CONTRIBUTING.md\n\nWrite tests.\n\nBe strict.");
    }

    #[tokio::test]
    async fn returns_empty_when_nothing_applies() {
        let dir = repo(&[("EMPTY.md", "  \n")]);
        assert_eq!(load_guidelines(dir.path(), &names(&["EMPTY.md", "none"]), "", LIMIT).await, "");
    }

    #[tokio::test]
    async fn truncates_on_char_boundary_with_note() {
        let dir = repo(&[("CLAUDE.md", "ab\u{e9}cd")]);
        let text = load_guidelines(dir.path(), &names(&["CLAUDE.md"]), "", 3).await;
        assert_eq!(text, "## CLAUDE.md\n\nab\n[truncated]");
    }

    #[tokio::test]
    async fn ignores_absolute_and_parent_paths() {
        let outer = repo(&[("secret.md", "secret"), ("repo/CLAUDE.md", "ok")]);
        let root = outer.path().join("repo");
        let absolute = outer.path().join("secret.md").to_string_lossy().into_owned();
        let files = vec!["../secret.md".to_string(), "sub/../../secret.md".to_string(), absolute, "".into()];

        assert_eq!(load_guidelines(&root, &files, "", LIMIT).await, "");
        assert_eq!(load_guidelines(&root, &names(&["./CLAUDE.md"]), "", LIMIT).await, "## ./CLAUDE.md\n\nok");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ignores_symlinks_and_directories() {
        let outer = repo(&[("secret.md", "secret"), ("repo/dir/x", "x")]);
        let root = outer.path().join("repo");
        std::os::unix::fs::symlink(outer.path().join("secret.md"), root.join("CLAUDE.md")).expect("symlink");
        std::os::unix::fs::symlink(outer.path(), root.join("escape")).expect("symlink");

        let files = names(&["CLAUDE.md", "dir", "escape/secret.md"]);
        assert_eq!(load_guidelines(&root, &files, "", LIMIT).await, "");
    }

    #[test]
    fn truncate_keeps_short_text_intact() {
        assert_eq!(truncate_on_char_boundary("abc", 3), "abc");
        assert_eq!(truncate_on_char_boundary("\u{e9}", 1), "");
    }
}
