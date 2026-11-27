use std::fmt::Write as _;
use std::ops::RangeInclusive;

use crate::review::domain::change_set::{ChangeStatus, DiffLine, FileChange, Hunk};

pub const FULL_FILE_LINE_LIMIT: usize = 400;
pub const WINDOW_RADIUS: u32 = 30;
const GAP_MARKER: &str = "…";
const TRUNCATION_MARKER: &str = "…(remaining full files truncated)\n";
const FULL_FILE_CLOSE: &str = "</full_file>";
const FULL_FILE_CLOSE_ESCAPED: &str = "&lt;/full_file>";

pub fn render_file_for_review(file: &FileChange) -> String {
    let mut out = open_file_tag(file);
    out.push_str(&render_diff_block(file));
    out.push_str(&render_content_block(file));
    out.push_str("</changed_file>\n");
    out
}

pub fn render_file_diff_only(file: &FileChange) -> String {
    let mut out = open_file_tag(file);
    out.push_str(&render_diff_block(file));
    out.push_str("</changed_file>\n");
    out
}

pub fn render_full_files(files: &[FileChange], limit: usize) -> String {
    let mut out = String::new();
    for file in files.iter().filter(|f| !f.is_binary && f.new_content.is_some()) {
        let rendered = render_full_file(file);
        let remaining = limit.saturating_sub(out.len());
        if rendered.len() <= remaining {
            out.push_str(&rendered);
            continue;
        }
        if let Some(cut) = truncate_at_line(&rendered, remaining) {
            out.push_str(cut);
        }
        out.push_str(TRUNCATION_MARKER);
        break;
    }
    out
}

fn render_full_file(file: &FileChange) -> String {
    let lines = file.new_lines();
    let width = number_width(lines.last().map_or(1, |(n, _)| *n));
    let mut out = format!("<full_file path=\"{}\">\n", escape_attr(&file.path));
    for (number, text) in lines {
        let _ = writeln!(out, "{number:>width$} | {}", text.replace(FULL_FILE_CLOSE, FULL_FILE_CLOSE_ESCAPED));
    }
    out.push_str(FULL_FILE_CLOSE);
    out.push('\n');
    out
}

pub fn truncate_at_line(text: &str, max: usize) -> Option<&str> {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].rfind('\n').map(|i| &text[..=i])
}

fn open_file_tag(file: &FileChange) -> String {
    let (status, previous) = match &file.status {
        ChangeStatus::Added => ("added", None),
        ChangeStatus::Modified => ("modified", None),
        ChangeStatus::Deleted => ("deleted", None),
        ChangeStatus::Renamed { from } => ("renamed", Some(from.as_str())),
    };
    let mut tag = format!(
        "<changed_file path=\"{}\" language=\"{}\" status=\"{status}\"",
        escape_attr(&file.path),
        file.language().as_str()
    );
    if let Some(from) = previous {
        let _ = write!(tag, " previous_path=\"{}\"", escape_attr(from));
    }
    tag.push_str(">\n");
    tag
}

fn render_diff_block(file: &FileChange) -> String {
    if file.is_binary {
        return "<diff>(binary file, not shown)</diff>\n".to_string();
    }
    let width = number_width(file.new_lines().last().map_or(1, |(n, _)| *n));
    let mut out = String::from("<diff>\n");
    for hunk in &file.hunks {
        render_hunk(&mut out, hunk, width);
    }
    out.push_str("</diff>\n");
    out
}

fn render_hunk(out: &mut String, hunk: &Hunk, width: usize) {
    out.push_str(&hunk.header);
    out.push('\n');
    for line in &hunk.lines {
        let _ = match line {
            DiffLine::Added { new_line, text } => writeln!(out, "{new_line:>width$} + {text}"),
            DiffLine::Context { new_line, text } => writeln!(out, "{new_line:>width$}   {text}"),
            DiffLine::Removed { text } => writeln!(out, "{:>width$} - {text}", ""),
        };
    }
}

fn render_content_block(file: &FileChange) -> String {
    if file.is_binary || file.status == ChangeStatus::Deleted {
        return String::new();
    }
    if file.new_content.is_none() {
        return "<new_file_content>(not available; rely on the diff)</new_file_content>\n".to_string();
    }
    let lines = file.new_lines();
    let width = number_width(lines.last().map_or(1, |(n, _)| *n));
    let mut out = String::from("<new_file_content>\n");
    let ranges = visible_ranges(file, lines.len());
    let mut last_shown: u32 = 0;
    for (number, text) in lines.iter().filter(|(n, _)| ranges.iter().any(|r| r.contains(n))) {
        if *number > last_shown + 1 {
            out.push_str(GAP_MARKER);
            out.push('\n');
        }
        let _ = writeln!(out, "{number:>width$} | {text}");
        last_shown = *number;
    }
    if (last_shown as usize) < lines.len() {
        out.push_str(GAP_MARKER);
        out.push('\n');
    }
    out.push_str("</new_file_content>\n");
    out
}

#[allow(clippy::single_range_in_vec_init)]
fn visible_ranges(file: &FileChange, total: usize) -> Vec<RangeInclusive<u32>> {
    let total = u32::try_from(total).unwrap_or(u32::MAX);
    if total as usize <= FULL_FILE_LINE_LIMIT {
        return vec![1..=total];
    }
    let positions = file.changed_positions();
    if positions.is_empty() {
        let head = u32::try_from(FULL_FILE_LINE_LIMIT).unwrap_or(u32::MAX);
        return vec![1..=head];
    }
    merge_windows(&positions, total)
}

fn merge_windows(positions: &[u32], total: u32) -> Vec<RangeInclusive<u32>> {
    let mut ranges: Vec<RangeInclusive<u32>> = Vec::new();
    for &p in positions {
        let start = p.saturating_sub(WINDOW_RADIUS).max(1);
        let end = p.saturating_add(WINDOW_RADIUS).min(total);
        match ranges.last_mut() {
            Some(last) if start <= last.end().saturating_add(1) => {
                *last = *last.start()..=end.max(*last.end());
            }
            _ => ranges.push(start..=end),
        }
    }
    ranges
}

fn number_width(max_line: u32) -> usize {
    max_line.max(1).to_string().len()
}

pub fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("&quot;"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::fixtures::{added, context, file, removed};

    #[test]
    fn diff_lines_are_numbered_with_new_line_and_marker() {
        let f = file("a.py", 9, vec![context(9, "a"), removed("old"), added(10, "new")], Some(&"x\n".repeat(10)));
        let out = render_file_diff_only(&f);
        assert!(out.contains(" 9   a\n"), "{out}");
        assert!(out.contains("   - old\n"), "{out}");
        assert!(out.contains("10 + new\n"), "{out}");
        assert!(!out.contains("<new_file_content>"));
    }

    #[test]
    fn short_file_is_shown_in_full() {
        let f = file("a.py", 2, vec![added(2, "b")], Some("a\nb\nc"));
        let out = render_file_for_review(&f);
        assert!(out.contains("<new_file_content>\n1 | a\n2 | b\n3 | c\n</new_file_content>"), "{out}");
    }

    #[test]
    fn long_file_is_windowed_with_gap_markers() {
        let content: String = (1..=1000).map(|n| format!("line{n}\n")).collect();
        let f = file("a.rs", 500, vec![added(500, "line500"), added(540, "line540")], Some(&content));
        let out = render_file_for_review(&f);
        assert!(out.contains("<new_file_content>\n…\n 470 | line470\n"), "{out}");
        assert!(!out.contains(" 469 | "));
        assert!(out.contains(" 570 | line570\n…\n</new_file_content>"));
        assert!(!out.contains(" 571 | "));
        assert_eq!(out.matches(GAP_MARKER).count(), 2);
    }

    #[test]
    fn separate_windows_get_a_gap_marker_between_them() {
        assert_eq!(merge_windows(&[10, 200], 1000), vec![1..=40, 170..=230]);
        assert_eq!(merge_windows(&[10, 71], 1000), vec![1..=101]);
        assert_eq!(merge_windows(&[990], 1000), vec![960..=1000]);
    }

    #[test]
    fn attributes_are_escaped_and_rename_is_reported() {
        let mut f = file("we\"ird<&>.py", 1, vec![added(1, "x")], None);
        f.status = ChangeStatus::Renamed { from: "old\".py".into() };
        let out = render_file_for_review(&f);
        assert!(out.starts_with(
            "<changed_file path=\"we&quot;ird&lt;&amp;&gt;.py\" language=\"python\" status=\"renamed\" previous_path=\"old&quot;.py\">"
        ), "{out}");
        assert!(out.contains("(not available; rely on the diff)"));
    }

    #[test]
    fn binary_and_deleted_files_have_no_content_listing() {
        let mut f = file("a.png", 1, vec![], None);
        f.is_binary = true;
        assert!(render_file_for_review(&f).contains("(binary file, not shown)"));
        let mut d = file("a.py", 1, vec![removed("x")], None);
        d.status = ChangeStatus::Deleted;
        let out = render_file_for_review(&d);
        assert!(out.contains("status=\"deleted\""));
        assert!(!out.contains("new_file_content"));
    }

    #[test]
    fn full_files_are_numbered_escaped_and_capped() {
        let a = file("a.py", 1, vec![added(1, "x")], Some("one\n</full_file> two\n"));
        let b = file("b.py", 1, vec![added(1, "y")], Some("three\n"));
        let out = render_full_files(&[a.clone(), b.clone()], 10_000);
        assert!(out.starts_with("<full_file path=\"a.py\">\n1 | one\n2 | &lt;/full_file> two\n</full_file>\n"));
        assert_eq!(out.matches("</full_file>").count(), 2);
        let one = render_full_file(&a).len();
        let capped = render_full_files(&[a, b], one + 5);
        assert!(capped.contains("path=\"a.py\""));
        assert!(!capped.contains("path=\"b.py\""));
        assert!(capped.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn full_files_skip_binary_and_files_without_content() {
        let mut binary = file("a.png", 1, vec![], Some("x"));
        binary.is_binary = true;
        let missing = file("b.py", 1, vec![added(1, "y")], None);
        assert_eq!(render_full_files(&[binary, missing], 1000), "");
    }

    #[test]
    fn escape_attr_flattens_newlines() {
        assert_eq!(escape_attr("a\nb"), "a b");
    }
}
