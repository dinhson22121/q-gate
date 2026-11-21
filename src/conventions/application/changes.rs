//! Translates the review context's changed files into the conventions model.
//!
//! The review context does not re-export its change-set types through `review::application`, so
//! this one adapter reads them from `review::domain::change_set` directly.

use std::fmt::Write as _;

use crate::conventions::domain::evidence::{ChangedFile, NewLine};
use crate::review::domain::change_set::{ChangeStatus, DiffLine, FileChange};

/// Diff text sent for one check, in characters; later files are left out beyond it.
pub const DIFF_CHAR_LIMIT: usize = 120_000;

/// Reviewable files (text, not deleted) with a numbered diff, up to [`DIFF_CHAR_LIMIT`].
pub fn changed_files(files: &[FileChange]) -> Vec<ChangedFile> {
    let mut used = 0;
    let mut out = Vec::new();
    for file in files.iter().filter(|f| !f.is_binary && f.status != ChangeStatus::Deleted && f.has_content_change()) {
        let diff = render_diff(file);
        if used + diff.len() > DIFF_CHAR_LIMIT {
            continue;
        }
        used += diff.len();
        out.push(ChangedFile { path: file.path.clone(), diff, new_lines: new_lines(file) });
    }
    out
}

fn added_numbers(file: &FileChange) -> Vec<u32> {
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter_map(|l| match l {
            DiffLine::Added { new_line, .. } => Some(*new_line),
            _ => None,
        })
        .collect()
}

fn new_lines(file: &FileChange) -> Vec<NewLine> {
    let added = added_numbers(file);
    file.new_lines()
        .into_iter()
        .map(|(number, text)| NewLine { number, text: text.to_string(), added: added.contains(&number) })
        .collect()
}

/// `<line> + code` for added, `<line>   code` for context, `      - code` for removed lines.
fn render_diff(file: &FileChange) -> String {
    let mut out = String::new();
    for hunk in &file.hunks {
        let _ = writeln!(out, "{}", hunk.header);
        for line in &hunk.lines {
            let _ = match line {
                DiffLine::Added { new_line, text } => writeln!(out, "{new_line:>5} + {text}"),
                DiffLine::Context { new_line, text } => writeln!(out, "{new_line:>5}   {text}"),
                DiffLine::Removed { text } => writeln!(out, "      - {text}"),
            };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::fixtures::{added, context, file, removed};

    #[test]
    fn renders_numbered_diff_and_marks_added_lines() {
        let change = file("a.rs", 1, vec![context(1, "fn a() {"), removed("old();"), added(2, "new();")], None);
        let files = changed_files(&[change]);
        assert_eq!(files[0].diff, "@@\n    1   fn a() {\n      - old();\n    2 + new();\n");
        assert_eq!(
            files[0].new_lines,
            vec![
                NewLine { number: 1, text: "fn a() {".into(), added: false },
                NewLine { number: 2, text: "new();".into(), added: true }
            ]
        );
    }

    #[test]
    fn skips_binary_deleted_and_oversized_files() {
        let mut binary = file("a.png", 1, vec![added(1, "x")], None);
        binary.is_binary = true;
        let mut deleted = file("b.rs", 1, vec![removed("x")], None);
        deleted.status = ChangeStatus::Deleted;
        let huge = file("c.rs", 1, vec![added(1, &"x".repeat(DIFF_CHAR_LIMIT))], None);
        let ok = file("d.rs", 1, vec![added(1, "let x = 1;")], None);
        let files = changed_files(&[binary, deleted, huge, ok]);
        assert_eq!(files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["d.rs"]);
    }
}
