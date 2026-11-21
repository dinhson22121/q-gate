//! Mechanical grounding: rule examples must quote files the model was shown, and violations must
//! quote code the change added. Anything else is dropped.

use std::collections::HashMap;

use globset::Glob;

use super::rulebook::{FileLine, MAX_EXAMPLES, RuleDraft};

/// Lines a quote may be away from the line the model reported.
pub const LINE_WINDOW: u32 = 3;
/// A quote shorter than this (non-whitespace chars) does not identify anything.
const MIN_QUOTE_CHARS: usize = 6;

/// One line of a changed file's new version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLine {
    pub number: u32,
    pub text: String,
    pub added: bool,
}

/// A changed file as the conventions check sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    /// Numbered diff text shown to the model.
    pub diff: String,
    pub new_lines: Vec<NewLine>,
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn quote_lines(quote: &str) -> Vec<String> {
    quote.lines().map(normalize).filter(|l| !l.is_empty()).collect()
}

fn is_substantial(quote: &str) -> bool {
    quote.chars().filter(|c| !c.is_whitespace()).count() >= MIN_QUOTE_CHARS
}

/// Finds `quote` (consecutive lines, whitespace-insensitive, each quote line contained in its file
/// line) starting within [`LINE_WINDOW`] of `line` in `lines` (`(number, text)` pairs in order).
/// Returns the index of the first matched entry.
fn find_quote(lines: &[(u32, &str)], line: u32, quote: &str) -> Option<usize> {
    let wanted = quote_lines(quote);
    if wanted.is_empty() || !is_substantial(quote) {
        return None;
    }
    let normalized: Vec<String> = lines.iter().map(|(_, text)| normalize(text)).collect();
    (0..lines.len()).filter(|&i| lines[i].0.abs_diff(line) <= LINE_WINDOW).find(|&start| {
        wanted.iter().enumerate().all(|(offset, q)| normalized.get(start + offset).is_some_and(|l| l.contains(q)))
    })
}

/// Line where the example's quote really is in `content`, if it is within the window.
pub fn locate_example(content: &str, example: &FileLine) -> Option<u32> {
    let lines: Vec<(u32, &str)> = (1u32..).zip(content.lines()).collect();
    find_quote(&lines, example.line, &example.quote).map(|i| lines[i].0)
}

/// Whether `quote` is in the new code near `line` and touches at least one added line.
pub fn quote_in_added_code(file: &ChangedFile, line: u32, quote: &str) -> bool {
    let lines: Vec<(u32, &str)> = file.new_lines.iter().map(|l| (l.number, l.text.as_str())).collect();
    find_quote(&lines, line, quote)
        .is_some_and(|start| file.new_lines[start..].iter().take(quote_lines(quote).len()).any(|l| l.added))
}

/// Cleans model drafts: examples must exist in `files` (path → content shown to the model; line
/// numbers are corrected to where the quote really is), at most [`MAX_EXAMPLES`] are kept, and a
/// rule without a verified example is dropped. Invalid scope globs are removed.
pub fn validate_drafts(drafts: Vec<RuleDraft>, files: &HashMap<String, String>) -> Vec<RuleDraft> {
    drafts
        .into_iter()
        .filter(|d| !d.title.trim().is_empty())
        .filter_map(|draft| {
            let examples: Vec<FileLine> = draft
                .examples
                .iter()
                .filter_map(|ex| {
                    let content = files.get(ex.file.trim_start_matches("./"))?;
                    let line = locate_example(content, ex)?;
                    Some(FileLine { file: ex.file.trim_start_matches("./").to_string(), line, quote: ex.quote.clone() })
                })
                .take(MAX_EXAMPLES)
                .collect();
            if examples.is_empty() {
                return None;
            }
            let scope_globs = draft.scope_globs.iter().filter(|g| Glob::new(g).is_ok()).cloned().collect();
            let lint_suggestion = draft.lint_suggestion.filter(|s| !s.trim().is_empty());
            Some(RuleDraft { title: draft.title.trim().to_string(), examples, scope_globs, lint_suggestion, ..draft })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conventions::domain::rulebook::RuleKind;

    fn example(file: &str, line: u32, quote: &str) -> FileLine {
        FileLine { file: file.into(), line, quote: quote.into() }
    }

    fn draft(title: &str, examples: Vec<FileLine>, globs: &[&str]) -> RuleDraft {
        RuleDraft {
            title: title.into(),
            description: String::new(),
            kind: RuleKind::Inferred,
            scope_globs: globs.iter().map(|s| s.to_string()).collect(),
            examples,
            lint_suggestion: Some("  ".into()),
        }
    }

    #[test]
    fn locates_quotes_near_the_reported_line() {
        let content = "a\nb\nlet repo = Repo::new();\nc\nreturn Err(e)?;\n";
        assert_eq!(locate_example(content, &example("f", 1, "let repo = Repo::new();")), Some(3));
        assert_eq!(locate_example(content, &example("f", 3, "let   repo = Repo::new();\nc")), Some(3));
        assert_eq!(locate_example(content, &example("f", 20, "let repo = Repo::new();")), None);
        assert_eq!(locate_example(content, &example("f", 3, "a")), None);
    }

    #[test]
    fn drops_rules_whose_examples_do_not_exist() {
        let files = HashMap::from([("src/a.rs".to_string(), "use crate::domain::User;\nfn x() {}\n".to_string())]);
        let kept = validate_drafts(
            vec![
                draft("real", vec![example("./src/a.rs", 2, "use crate::domain::User;")], &["src/**", "["]),
                draft("invented quote", vec![example("src/a.rs", 1, "use crate::infra::Db;")], &[]),
                draft("unknown file", vec![example("src/zzz.rs", 1, "use crate::domain::User;")], &[]),
                draft("  ", vec![example("src/a.rs", 1, "use crate::domain::User;")], &[]),
            ],
            &files,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].examples, vec![example("src/a.rs", 1, "use crate::domain::User;")]);
        assert_eq!(kept[0].scope_globs, vec!["src/**".to_string()]);
        assert_eq!(kept[0].lint_suggestion, None);
    }

    #[test]
    fn violation_quotes_must_touch_added_lines() {
        let line = |number, text: &str, added| NewLine { number, text: text.into(), added };
        let file = ChangedFile {
            path: "a.rs".into(),
            diff: String::new(),
            new_lines: vec![line(1, "use std::fs;", false), line(2, "let db = Db::connect();", true)],
        };
        assert!(quote_in_added_code(&file, 2, "let db = Db::connect();"));
        assert!(quote_in_added_code(&file, 1, "use std::fs;\nlet db = Db::connect();"));
        assert!(!quote_in_added_code(&file, 1, "use std::fs;"));
        assert!(!quote_in_added_code(&file, 2, "let db = Other::connect();"));
    }
}
