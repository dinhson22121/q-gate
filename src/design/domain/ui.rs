//! Which changed files are UI code, and which lines the checks may look at.

use std::collections::HashSet;

use crate::review::domain::change_set::{DiffLine, FileChange};

/// Markup / component files: every check runs on them.
const UI_EXTENSIONS: [&str; 7] = ["tsx", "jsx", "ts", "js", "vue", "svelte", "html"];
/// Stylesheets: only the token check runs on them.
const STYLE_EXTENSIONS: [&str; 4] = ["css", "scss", "sass", "less"];
const I18N_DIRS: [&str; 4] = ["locales", "i18n", "lang", "translations"];
const I18N_EXTENSIONS: [&str; 3] = ["json", "yaml", "yml"];
/// Marker that suppresses design mismatches on its own line and the line below.
pub const IGNORE_MARKER: &str = "qgate-design-ignore";

fn extension(path: &str) -> String {
    path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default()
}

pub fn is_ui_file(path: &str) -> bool {
    UI_EXTENSIONS.contains(&extension(path).as_str()) && !is_test_or_story(path)
}

pub fn is_style_file(path: &str) -> bool {
    STYLE_EXTENSIONS.contains(&extension(path).as_str())
}

/// Test, story and type-declaration files render nothing a user sees.
fn is_test_or_story(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or_default();
    [".test.", ".spec.", ".stories.", ".d.ts"].iter().any(|m| name.contains(m))
        || lower.split('/').any(|dir| dir == "__tests__")
}

/// json/yaml files below a `locales`, `i18n`, `lang` or `translations` directory.
pub fn is_i18n_resource(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut dirs = lower.split('/').rev().skip(1);
    I18N_EXTENSIONS.contains(&extension(&lower).as_str()) && dirs.any(|d| I18N_DIRS.contains(&d))
}

/// Changed, non-binary, non-deleted files with content changes.
pub fn reviewable(file: &FileChange) -> bool {
    !file.is_binary && file.has_content_change()
}

pub fn changed_ui_files(files: &[FileChange]) -> Vec<&FileChange> {
    files.iter().filter(|f| reviewable(f) && is_ui_file(&f.path)).collect()
}

/// Lines added by the change, with new-file numbers.
pub fn added_lines(file: &FileChange) -> Vec<(u32, &str)> {
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter_map(|line| match line {
            DiffLine::Added { new_line, text } => Some((*new_line, text.as_str())),
            _ => None,
        })
        .collect()
}

/// Line numbers suppressed by `qgate-design-ignore` (the marker line and the one after it).
pub fn ignored_lines(file: &FileChange) -> HashSet<u32> {
    file.new_lines()
        .into_iter()
        .filter(|(_, text)| text.contains(IGNORE_MARKER))
        .flat_map(|(number, _)| [number, number + 1])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_ui_style_and_i18n_paths() {
        assert!(is_ui_file("src/Pay.tsx"));
        assert!(is_ui_file("web/index.HTML"));
        assert!(!is_ui_file("src/Pay.test.tsx"));
        assert!(!is_ui_file("src/__tests__/pay.ts"));
        assert!(!is_ui_file("src/types.d.ts"));
        assert!(!is_ui_file("main.go"));
        assert!(is_style_file("a/b.module.scss"));
        assert!(is_i18n_resource("public/locales/en/common.json"));
        assert!(is_i18n_resource("src/i18n/vi.yml"));
        assert!(!is_i18n_resource("src/locales.json"));
        assert!(!is_i18n_resource("locales/en.ts"));
    }
}
