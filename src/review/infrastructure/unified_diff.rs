use crate::review::domain::change_set::{ChangeStatus, DiffLine, FileChange, Hunk};

const FILE_HEADER: &str = "diff --git ";
const OLD_PATH: &str = "--- ";
const NEW_PATH: &str = "+++ ";
const NEW_FILE: &str = "new file mode";
const DELETED_FILE: &str = "deleted file mode";
const RENAME_FROM: &str = "rename from ";
const RENAME_TO: &str = "rename to ";
const BINARY_PATCH: &str = "GIT binary patch";
const HUNK_MARKER: &str = "@@";
const NO_NEWLINE_MARKER: char = '\\';
const DEV_NULL: &str = "/dev/null";
const OLD_PREFIX: &str = "a/";
const NEW_PREFIX: &str = "b/";
const DEFAULT_RANGE_COUNT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiffParseError {
    #[error("line {line}: malformed hunk header `{text}`")]
    MalformedHunkHeader { line: usize, text: String },
    #[error("line {line}: hunk found outside of a file section")]
    HunkOutsideFile { line: usize },
    #[error("line {line}: unexpected line inside hunk `{text}`")]
    UnexpectedHunkLine { line: usize, text: String },
    #[error("hunk starting at line {line} ends before its declared length")]
    TruncatedHunk { line: usize },
    #[error("line {line}: invalid quoted path `{text}`")]
    InvalidQuotedPath { line: usize, text: String },
    #[error("line {line}: cannot determine the file path")]
    MissingPath { line: usize },
}

pub fn parse_unified_diff(input: &str) -> Result<Vec<FileChange>, DiffParseError> {
    let mut lines = input.lines().enumerate().map(|(i, l)| (i + 1, l)).peekable();
    let mut files = Vec::new();
    let mut current: Option<FileBuilder> = None;

    while let Some((number, line)) = lines.next() {
        if let Some(rest) = line.strip_prefix(FILE_HEADER) {
            if let Some(done) = current.take() {
                files.push(done.finish()?);
            }
            current = Some(FileBuilder::new(number, rest));
            continue;
        }
        let Some(file) = current.as_mut() else {
            if line.starts_with(HUNK_MARKER) {
                return Err(DiffParseError::HunkOutsideFile { line: number });
            }
            continue;
        };
        if file.in_binary_patch {
            continue;
        }
        if line.starts_with(HUNK_MARKER) {
            let header = parse_hunk_header(number, line)?;
            file.hunks.push(read_hunk_body(number, header, &mut lines)?);
        } else {
            file.apply_header_line(number, line)?;
        }
    }
    if let Some(done) = current {
        files.push(done.finish()?);
    }
    Ok(files)
}

struct FileBuilder {
    line: usize,
    git_header: String,
    old_path: Option<String>,
    new_path: Option<String>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    is_new: bool,
    is_deleted: bool,
    is_binary: bool,
    in_binary_patch: bool,
    hunks: Vec<Hunk>,
}

impl FileBuilder {
    fn new(line: usize, git_header: &str) -> Self {
        Self {
            line,
            git_header: git_header.to_string(),
            old_path: None,
            new_path: None,
            rename_from: None,
            rename_to: None,
            is_new: false,
            is_deleted: false,
            is_binary: false,
            in_binary_patch: false,
            hunks: Vec::new(),
        }
    }

    fn apply_header_line(&mut self, number: usize, line: &str) -> Result<(), DiffParseError> {
        if line.starts_with(NEW_FILE) {
            self.is_new = true;
        } else if line.starts_with(DELETED_FILE) {
            self.is_deleted = true;
        } else if let Some(path) = line.strip_prefix(RENAME_FROM) {
            self.rename_from = Some(parse_path(number, path)?);
        } else if let Some(path) = line.strip_prefix(RENAME_TO) {
            self.rename_to = Some(parse_path(number, path)?);
        } else if line == BINARY_PATCH {
            self.is_binary = true;
            self.in_binary_patch = true;
        } else if line.starts_with("Binary files ") && line.ends_with(" differ") {
            self.is_binary = true;
        } else if let Some(path) = line.strip_prefix(OLD_PATH) {
            self.old_path = side_path(number, path, OLD_PREFIX)?;
            self.is_new |= self.old_path.is_none();
        } else if let Some(path) = line.strip_prefix(NEW_PATH) {
            self.new_path = side_path(number, path, NEW_PREFIX)?;
            self.is_deleted |= self.new_path.is_none();
        }
        Ok(())
    }

    fn finish(self) -> Result<FileChange, DiffParseError> {
        let (header_old, header_new) = split_git_header(self.line, &self.git_header)?;
        let old = self.rename_from.or(self.old_path).or(header_old);
        let new = self.rename_to.or(self.new_path).or(header_new);
        let (path, status) = if self.is_deleted {
            (old, ChangeStatus::Deleted)
        } else if self.is_new {
            (new, ChangeStatus::Added)
        } else {
            let status = match (&old, &new) {
                (Some(from), Some(to)) if from != to => ChangeStatus::Renamed { from: from.clone() },
                _ => ChangeStatus::Modified,
            };
            (new, status)
        };
        Ok(FileChange {
            path: path.ok_or(DiffParseError::MissingPath { line: self.line })?,
            status,
            is_binary: self.is_binary,
            hunks: self.hunks,
            new_content: None,
        })
    }
}

struct HunkHeader {
    text: String,
    old_count: u32,
    new_start: u32,
    new_count: u32,
}

fn parse_hunk_header(number: usize, line: &str) -> Result<HunkHeader, DiffParseError> {
    let malformed = || DiffParseError::MalformedHunkHeader { line: number, text: line.to_string() };
    let ranges = line
        .strip_prefix("@@ ")
        .and_then(|rest| rest.split_once(" @@"))
        .map(|(ranges, _section)| ranges)
        .ok_or_else(malformed)?;
    let (old, new) = ranges.split_once(' ').ok_or_else(malformed)?;
    let (_old_start, old_count) = old.strip_prefix('-').and_then(parse_range).ok_or_else(malformed)?;
    let (new_start, new_count) = new.strip_prefix('+').and_then(parse_range).ok_or_else(malformed)?;
    Ok(HunkHeader { text: line.to_string(), old_count, new_start, new_count })
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((range.parse().ok()?, DEFAULT_RANGE_COUNT)),
    }
}

fn read_hunk_body<'a>(
    header_line: usize,
    header: HunkHeader,
    lines: &mut impl Iterator<Item = (usize, &'a str)>,
) -> Result<Hunk, DiffParseError> {
    let mut old_left = header.old_count;
    let mut new_left = header.new_count;
    let mut new_line = header.new_start;
    let mut body = Vec::new();

    while old_left > 0 || new_left > 0 {
        let (number, line) = lines.next().ok_or(DiffParseError::TruncatedHunk { line: header_line })?;
        let mut chars = line.chars();
        let marker = chars.next();
        let text = chars.as_str().to_string();
        match marker {
            Some('-') if old_left > 0 => {
                old_left -= 1;
                body.push(DiffLine::Removed { text });
            }
            Some('+') if new_left > 0 => {
                new_left -= 1;
                body.push(DiffLine::Added { new_line, text });
                new_line += 1;
            }
            Some(' ') | None if old_left > 0 && new_left > 0 => {
                old_left -= 1;
                new_left -= 1;
                body.push(DiffLine::Context { new_line, text });
                new_line += 1;
            }
            Some(NO_NEWLINE_MARKER) => {}
            _ => {
                return Err(DiffParseError::UnexpectedHunkLine { line: number, text: line.to_string() });
            }
        }
    }
    Ok(Hunk { header: header.text, new_start: header.new_start, lines: body })
}

fn side_path(number: usize, raw: &str, prefix: &str) -> Result<Option<String>, DiffParseError> {
    let raw = raw.strip_suffix('\t').unwrap_or(raw);
    if raw == DEV_NULL {
        return Ok(None);
    }
    let path = parse_path(number, raw)?;
    Ok(Some(path.strip_prefix(prefix).map(str::to_string).unwrap_or(path)))
}

fn split_git_header(number: usize, rest: &str) -> Result<(Option<String>, Option<String>), DiffParseError> {
    let (old, new) = if rest.starts_with('"') {
        let end = closing_quote(rest).ok_or_else(|| invalid_path(number, rest))?;
        (parse_path(number, &rest[..=end])?, parse_path(number, rest[end + 1..].trim_start())?)
    } else if let Some(start) = rest.find(" \"") {
        (rest[..start].to_string(), parse_path(number, &rest[start + 1..])?)
    } else if let Some(same) = symmetric_header_path(rest) {
        (format!("{OLD_PREFIX}{same}"), format!("{NEW_PREFIX}{same}"))
    } else {
        match rest.rsplit_once(" b/") {
            Some((old, new)) => (old.to_string(), format!("{NEW_PREFIX}{new}")),
            None => return Ok((None, None)),
        }
    };
    let strip = |path: String, prefix: &str| path.strip_prefix(prefix).map(str::to_string).unwrap_or(path);
    Ok((Some(strip(old, OLD_PREFIX)), Some(strip(new, NEW_PREFIX))))
}

fn symmetric_header_path(rest: &str) -> Option<&str> {
    let name_len = rest.len().checked_sub(OLD_PREFIX.len() + NEW_PREFIX.len() + 1)? / 2;
    let old = rest.strip_prefix(OLD_PREFIX)?.get(..name_len)?;
    let new = rest.get(OLD_PREFIX.len() + name_len..)?.strip_prefix(' ')?.strip_prefix(NEW_PREFIX)?;
    (old == new).then_some(old)
}

fn closing_quote(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(1) {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(i),
            _ => {}
        }
    }
    None
}

fn parse_path(number: usize, raw: &str) -> Result<String, DiffParseError> {
    match raw.strip_prefix('"') {
        Some(quoted) => {
            let inner = quoted.strip_suffix('"').ok_or_else(|| invalid_path(number, raw))?;
            unquote(inner).ok_or_else(|| invalid_path(number, raw))
        }
        None => Ok(raw.to_string()),
    }
}

fn unquote(inner: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(inner.len());
    let mut input = inner.bytes();
    while let Some(b) = input.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        let decoded = match input.next()? {
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            digit @ b'0'..=b'7' => {
                let digits = [digit, input.next()?, input.next()?];
                u8::from_str_radix(std::str::from_utf8(&digits).ok()?, 8).ok()?
            }
            other => other,
        };
        bytes.push(decoded);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn invalid_path(line: usize, text: &str) -> DiffParseError {
    DiffParseError::InvalidQuotedPath { line, text: text.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::fixtures::{added, context, removed};

    fn parse_one(input: &str) -> FileChange {
        let mut files = parse_unified_diff(input).expect("diff parses");
        assert_eq!(files.len(), 1, "expected exactly one file");
        files.remove(0)
    }

    #[test]
    fn parses_modified_file_with_line_numbers() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,3 +10,4 @@ fn main() {
 keep
-old
+new
+extra
 tail
";
        let file = parse_one(diff);
        assert_eq!(file.path, "src/lib.rs");
        assert_eq!(file.status, ChangeStatus::Modified);
        assert!(!file.is_binary);
        assert_eq!(file.new_content, None);
        assert_eq!(file.hunks.len(), 1);
        let hunk = &file.hunks[0];
        assert_eq!(hunk.header, "@@ -10,3 +10,4 @@ fn main() {");
        assert_eq!(hunk.new_start, 10);
        assert_eq!(
            hunk.lines,
            vec![context(10, "keep"), removed("old"), added(11, "new"), added(12, "extra"), context(13, "tail")]
        );
    }

    #[test]
    fn parses_added_file() {
        let diff = "\
diff --git a/new.py b/new.py
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.py
@@ -0,0 +1,2 @@
+a
+b
";
        let file = parse_one(diff);
        assert_eq!(file.path, "new.py");
        assert_eq!(file.status, ChangeStatus::Added);
        assert_eq!(file.hunks[0].lines, vec![added(1, "a"), added(2, "b")]);
    }

    #[test]
    fn parses_deleted_file_using_old_path() {
        let diff = "\
diff --git a/gone.go b/gone.go
deleted file mode 100644
index 4444444..0000000
--- a/gone.go
+++ /dev/null
@@ -1,2 +0,0 @@
-x
-y
";
        let file = parse_one(diff);
        assert_eq!(file.path, "gone.go");
        assert_eq!(file.status, ChangeStatus::Deleted);
        assert_eq!(file.hunks[0].lines, vec![removed("x"), removed("y")]);
        assert_eq!(file.hunks[0].new_start, 0);
    }

    #[test]
    fn parses_pure_rename_without_hunks() {
        let diff = "\
diff --git a/old name.rs b/new name.rs
similarity index 100%
rename from old name.rs
rename to new name.rs
";
        let file = parse_one(diff);
        assert_eq!(file.path, "new name.rs");
        assert_eq!(file.status, ChangeStatus::Renamed { from: "old name.rs".into() });
        assert!(file.hunks.is_empty());
    }

    #[test]
    fn parses_rename_with_changes() {
        let diff = "\
diff --git a/a.rs b/b.rs
similarity index 90%
rename from a.rs
rename to b.rs
index 1..2 100644
--- a/a.rs
+++ b/b.rs
@@ -1 +1 @@
-x
+y
";
        let file = parse_one(diff);
        assert_eq!(file.path, "b.rs");
        assert_eq!(file.status, ChangeStatus::Renamed { from: "a.rs".into() });
        assert_eq!(file.hunks[0].lines, vec![removed("x"), added(1, "y")]);
    }

    #[test]
    fn detects_binary_files_in_both_formats() {
        let diff = "\
diff --git a/img.png b/img.png
index 1..2 100644
Binary files a/img.png and b/img.png differ
diff --git a/blob.bin b/blob.bin
new file mode 100644
index 0000000..5555555
GIT binary patch
literal 4
LcmZQzWMT#Y01f~L

literal 0
HcmV?d00001

diff --git a/c.txt b/c.txt
--- a/c.txt
+++ b/c.txt
@@ -1 +1 @@
-a
+b
";
        let files = parse_unified_diff(diff).expect("diff parses");
        assert_eq!(files.len(), 3);
        assert!(files[0].is_binary);
        assert_eq!(files[0].path, "img.png");
        assert_eq!(files[0].status, ChangeStatus::Modified);
        assert!(files[1].is_binary);
        assert_eq!(files[1].path, "blob.bin");
        assert_eq!(files[1].status, ChangeStatus::Added);
        assert!(files[1].hunks.is_empty());
        assert!(!files[2].is_binary);
        assert_eq!(files[2].hunks[0].lines, vec![removed("a"), added(1, "b")]);
    }

    #[test]
    fn parses_multiple_hunks_and_files() {
        let diff = "\
diff --git a/a.ts b/a.ts
--- a/a.ts
+++ b/a.ts
@@ -1,2 +1,2 @@
-one
+uno
 two
@@ -20,0 +21,1 @@
+inserted
diff --git a/b.ts b/b.ts
--- a/b.ts
+++ b/b.ts
@@ -5,1 +5,0 @@
-dropped
";
        let files = parse_unified_diff(diff).expect("diff parses");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[1].new_start, 21);
        assert_eq!(files[0].hunks[1].lines, vec![added(21, "inserted")]);
        assert_eq!(files[1].path, "b.ts");
        assert_eq!(files[1].hunks[0].lines, vec![removed("dropped")]);
    }

    #[test]
    fn skips_no_newline_markers() {
        let diff = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
        let file = parse_one(diff);
        assert_eq!(file.hunks[0].lines, vec![removed("old"), added(1, "new")]);
    }

    #[test]
    fn removed_lines_that_look_like_headers_stay_in_the_hunk() {
        let diff = "\
diff --git a/q.sql b/q.sql
--- a/q.sql
+++ b/q.sql
@@ -1,3 +1,2 @@
--- a comment
-++ odd
+select 1;
 end
";
        let file = parse_one(diff);
        assert_eq!(
            file.hunks[0].lines,
            vec![removed("-- a comment"), removed("++ odd"), added(1, "select 1;"), context(2, "end")]
        );
        assert_eq!(file.path, "q.sql");
    }

    #[test]
    fn treats_bare_empty_line_as_empty_context() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n\n-x\n+y\n";
        let file = parse_one(diff);
        assert_eq!(file.hunks[0].lines, vec![context(1, ""), removed("x"), added(2, "y")]);
    }

    #[test]
    fn unquotes_git_quoted_paths() {
        let diff = "\
diff --git \"a/t\\303\\251st \\\"q\\\".txt\" \"b/t\\303\\251st \\\"q\\\".txt\"
new file mode 100644
--- /dev/null
+++ \"b/t\\303\\251st \\\"q\\\".txt\"
@@ -0,0 +1 @@
+hi
";
        let file = parse_one(diff);
        assert_eq!(file.path, "t\u{e9}st \"q\".txt");
    }

    #[test]
    fn strips_trailing_tab_from_paths_with_spaces() {
        let diff = "diff --git a/my file b/my file\n--- a/my file\t\n+++ b/my file\t\n@@ -1 +1 @@\n-a\n+b\n";
        let file = parse_one(diff);
        assert_eq!(file.path, "my file");
        assert_eq!(file.status, ChangeStatus::Modified);
    }

    #[test]
    fn mode_only_change_uses_git_header_path() {
        let diff = "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        let file = parse_one(diff);
        assert_eq!(file.path, "run.sh");
        assert_eq!(file.status, ChangeStatus::Modified);
        assert!(!file.has_content_change());
    }

    #[test]
    fn empty_input_yields_no_files() {
        assert_eq!(parse_unified_diff(""), Ok(Vec::new()));
    }

    #[test]
    fn rejects_malformed_hunk_header() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -x +1 @@\n+a\n";
        assert!(matches!(parse_unified_diff(diff), Err(DiffParseError::MalformedHunkHeader { line: 4, .. })));
        let missing_close = "diff --git a/a b/a\n@@ -1 +1\n";
        assert!(matches!(parse_unified_diff(missing_close), Err(DiffParseError::MalformedHunkHeader { .. })));
    }

    #[test]
    fn rejects_hunk_before_any_file() {
        assert_eq!(parse_unified_diff("@@ -1 +1 @@\n-a\n+b\n"), Err(DiffParseError::HunkOutsideFile { line: 1 }));
    }

    #[test]
    fn rejects_truncated_hunk() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n-a\n";
        assert_eq!(parse_unified_diff(diff), Err(DiffParseError::TruncatedHunk { line: 4 }));
    }

    #[test]
    fn rejects_lines_exceeding_declared_counts() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1,2 @@\n-a\n-b\n";
        assert!(matches!(parse_unified_diff(diff), Err(DiffParseError::UnexpectedHunkLine { line: 6, .. })));
    }

    #[test]
    fn rejects_unterminated_quoted_path() {
        let diff = "diff --git \"a/x b/x\n";
        assert!(matches!(parse_unified_diff(diff), Err(DiffParseError::InvalidQuotedPath { .. })));
    }

    #[test]
    fn falls_back_to_last_b_prefix_for_asymmetric_header() {
        assert_eq!(split_git_header(1, "a/x y b/z"), Ok((Some("x y".to_string()), Some("z".to_string()))));
        assert_eq!(split_git_header(1, "weird"), Ok((None, None)));
    }

    #[test]
    fn unquote_handles_escapes_and_rejects_bad_sequences() {
        assert_eq!(unquote(r"a\tb\\c\nd\a\b\f\r\v"), Some("a\tb\\c\nd\u{7}\u{8}\u{c}\r\u{b}".into()));
        assert_eq!(unquote(r"bad\"), None);
        assert_eq!(unquote(r"\12"), None);
    }
}
