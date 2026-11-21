//! Picks a small, representative sample of source code per language for deriving inferred rules.

use std::collections::{BTreeMap, HashMap};

use super::sources::is_ignored_path;

/// Files kept per language.
pub const SAMPLES_PER_LANGUAGE: usize = 12;
/// Bytes kept per sampled file.
pub const SAMPLE_FILE_CAP: usize = 4 * 1024;
/// Bytes of sampled code sent in total.
pub const SAMPLE_TOTAL_CAP: usize = 80 * 1024;
/// Files read per (language, top directory) to rank by imports; bounds I/O on huge repositories.
const CANDIDATES_PER_GROUP: usize = 6;
/// Files read in total for ranking.
const MAX_CANDIDATES: usize = 300;

const LANGUAGES: [(&str, &[&str]); 12] = [
    ("rust", &["rs"]),
    ("go", &["go"]),
    ("java", &["java"]),
    ("kotlin", &["kt", "kts"]),
    ("python", &["py"]),
    ("typescript", &["ts", "tsx"]),
    ("javascript", &["js", "jsx", "mjs"]),
    ("csharp", &["cs"]),
    ("ruby", &["rb"]),
    ("php", &["php"]),
    ("scala", &["scala"]),
    ("swift", &["swift"]),
];

const IMPORT_PREFIXES: [&str; 7] = ["use ", "import ", "from ", "require ", "#include", "using ", "package "];

/// A code file read for ranking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeCandidate {
    pub path: String,
    pub import_count: usize,
}

/// Language name by file extension, for code files worth sampling.
pub fn code_language(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1;
    LANGUAGES.iter().find(|(_, exts)| exts.contains(&ext)).map(|(name, _)| *name)
}

fn top_dir(path: &str) -> &str {
    path.split_once('/').map_or("", |(top, _)| top)
}

/// Generated or minified files tell nothing about conventions.
fn is_noise(path: &str) -> bool {
    path.contains(".min.") || path.contains(".generated.") || path.ends_with(".d.ts") || path.contains("/generated/")
}

/// Code files to read for ranking: a few per (language, top dir), bounded overall.
pub fn candidate_paths(paths: &[String]) -> Vec<String> {
    let mut groups: BTreeMap<(&str, &str), Vec<&String>> = BTreeMap::new();
    for path in paths.iter().filter(|p| !is_ignored_path(p) && !is_noise(p)) {
        if let Some(language) = code_language(path) {
            groups.entry((language, top_dir(path))).or_default().push(path);
        }
    }
    let mut picked = Vec::new();
    for files in groups.values_mut() {
        files.sort_by_key(|p| (p.matches('/').count(), p.as_str()));
        picked.extend(files.iter().take(CANDIDATES_PER_GROUP).map(|p| (*p).clone()));
    }
    picked.truncate(MAX_CANDIDATES);
    picked
}

/// Import/use statements, a cheap proxy for how much of the codebase's structure a file shows.
pub fn count_imports(text: &str) -> usize {
    text.lines().map(str::trim_start).filter(|l| IMPORT_PREFIXES.iter().any(|p| l.starts_with(p))).count()
}

/// Up to [`SAMPLES_PER_LANGUAGE`] files per language, spread across top directories (most imports
/// first within a directory), interleaved across languages so a size budget cut keeps every language.
pub fn choose_samples(candidates: &[CodeCandidate]) -> Vec<String> {
    let mut by_language: BTreeMap<&str, BTreeMap<&str, Vec<&CodeCandidate>>> = BTreeMap::new();
    for candidate in candidates {
        if let Some(language) = code_language(&candidate.path) {
            by_language.entry(language).or_default().entry(top_dir(&candidate.path)).or_default().push(candidate);
        }
    }
    let mut per_language: Vec<Vec<String>> = by_language.into_values().map(spread_across_dirs).collect();
    // Biggest language first.
    per_language.sort_by_key(|files| std::cmp::Reverse(files.len()));
    let mut out = Vec::new();
    for round in 0..SAMPLES_PER_LANGUAGE {
        out.extend(per_language.iter().filter_map(|files| files.get(round).cloned()));
    }
    out
}

fn spread_across_dirs(mut dirs: BTreeMap<&str, Vec<&CodeCandidate>>) -> Vec<String> {
    for files in dirs.values_mut() {
        files.sort_by(|a, b| b.import_count.cmp(&a.import_count).then_with(|| a.path.cmp(&b.path)));
    }
    let mut out = Vec::new();
    let mut round = 0;
    while out.len() < SAMPLES_PER_LANGUAGE {
        let before = out.len();
        out.extend(dirs.values().filter_map(|files| files.get(round)).map(|c| c.path.clone()));
        if out.len() == before {
            break;
        }
        round += 1;
    }
    out.truncate(SAMPLES_PER_LANGUAGE);
    out
}

/// Cuts `text` to at most `cap` bytes on a line boundary when possible (never inside a char).
pub fn truncate_text(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &text[..end];
    cut.rfind('\n').map_or(cut, |nl| &cut[..=nl])
}

/// Keeps samples in order until the total budget is used up.
pub fn within_budget<T>(items: Vec<(T, usize)>, budget: usize) -> Vec<T> {
    let mut used = 0;
    let mut out = Vec::new();
    for (item, size) in items {
        if used + size > budget {
            continue;
        }
        used += size;
        out.push(item);
    }
    out
}

/// Number of files per language, for the prompt's overview.
pub fn language_counts(paths: &[String]) -> HashMap<&'static str, usize> {
    let mut counts = HashMap::new();
    for language in paths.iter().filter(|p| !is_ignored_path(p)).filter_map(|p| code_language(p)) {
        *counts.entry(language).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, import_count: usize) -> CodeCandidate {
        CodeCandidate { path: path.into(), import_count }
    }

    #[test]
    fn detects_languages_and_skips_noise() {
        assert_eq!(code_language("a/b.rs"), Some("rust"));
        assert_eq!(code_language("x.tsx"), Some("typescript"));
        assert_eq!(code_language("Makefile"), None);
        let paths: Vec<String> =
            ["src/a.rs", "web/app.min.js", "types.d.ts", "node_modules/x/y.js", "src/b.py"].map(String::from).into();
        assert_eq!(candidate_paths(&paths), vec!["src/b.py".to_string(), "src/a.rs".to_string()]);
    }

    #[test]
    fn candidates_are_bounded_per_group() {
        let paths: Vec<String> = (0..20).map(|i| format!("src/f{i:02}.rs")).collect();
        assert_eq!(candidate_paths(&paths).len(), CANDIDATES_PER_GROUP);
    }

    #[test]
    fn counts_imports() {
        assert_eq!(count_imports("use a;\n  use b::c;\nfn x() {}\nimport java.util.List;\nfrom x import y\n"), 4);
    }

    #[test]
    fn samples_spread_over_dirs_prefer_imports_and_interleave_languages() {
        let mut candidates = vec![
            candidate("api/a.rs", 1),
            candidate("api/b.rs", 9),
            candidate("core/c.rs", 2),
            candidate("tools/x.py", 3),
        ];
        candidates.extend((0..20).map(|i| candidate(&format!("lib/l{i:02}.rs"), 0)));
        let picked = choose_samples(&candidates);
        assert_eq!(picked[0], "api/b.rs");
        assert_eq!(picked[1], "tools/x.py");
        assert!(picked.contains(&"core/c.rs".to_string()));
        assert_eq!(picked.iter().filter(|p| p.ends_with(".rs")).count(), SAMPLES_PER_LANGUAGE);
    }

    #[test]
    fn truncation_and_budget() {
        assert_eq!(truncate_text("abc\ndef\nghi", 9), "abc\ndef\n");
        assert_eq!(truncate_text("abcdef", 3), "abc");
        assert_eq!(truncate_text("é", 1), "");
        assert_eq!(truncate_text("short", 10), "short");
        assert_eq!(within_budget(vec![("a", 5), ("b", 6), ("c", 4)], 10), vec!["a", "c"]);
        let counts = language_counts(&["a.rs".into(), "b.rs".into(), "c.go".into()]);
        assert_eq!(counts["rust"], 2);
    }
}
