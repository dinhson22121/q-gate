use super::language::Language;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed { from: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Added { new_line: u32, text: String },
    Removed { text: String },
    Context { new_line: u32, text: String },
}

impl DiffLine {
    fn text(&self) -> &str {
        match self {
            Self::Added { text, .. } | Self::Removed { text } | Self::Context { text, .. } => text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    /// First line number of this hunk in the new file version.
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

/// One file touched by a change, as seen after the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub status: ChangeStatus,
    pub is_binary: bool,
    pub hunks: Vec<Hunk>,
    /// Full content of the new file version, when it could be read.
    pub new_content: Option<String>,
}

impl FileChange {
    pub fn language(&self) -> Language {
        Language::from_path(&self.path)
    }

    pub fn has_content_change(&self) -> bool {
        self.hunks.iter().any(|h| !h.lines.is_empty())
    }

    /// New-file line numbers where code was added, plus the positions where code was removed.
    pub fn changed_positions(&self) -> Vec<u32> {
        let mut positions = Vec::new();
        for hunk in &self.hunks {
            let mut cursor = hunk.new_start.max(1);
            for line in &hunk.lines {
                match line {
                    DiffLine::Added { new_line, .. } => {
                        positions.push(*new_line);
                        cursor = new_line + 1;
                    }
                    DiffLine::Context { new_line, .. } => cursor = new_line + 1,
                    DiffLine::Removed { .. } => positions.push(cursor),
                }
            }
        }
        positions.sort_unstable();
        positions.dedup();
        positions
    }

    /// Removed lines whose removal point (in new-file numbering) is within `window` of `line`.
    pub fn removed_lines_near(&self, line: u32, window: u32) -> Vec<&str> {
        let mut removed = Vec::new();
        for hunk in &self.hunks {
            let mut cursor = hunk.new_start.max(1);
            for diff_line in &hunk.lines {
                match diff_line {
                    DiffLine::Added { new_line, .. } | DiffLine::Context { new_line, .. } => cursor = new_line + 1,
                    DiffLine::Removed { text } if cursor.abs_diff(line) <= window => removed.push(text.as_str()),
                    DiffLine::Removed { .. } => {}
                }
            }
        }
        removed
    }

    pub fn is_near_change(&self, line: u32, tolerance: u32) -> bool {
        self.changed_positions().iter().any(|p| p.abs_diff(line) <= tolerance)
    }

    /// Lines of the new file version with 1-based numbers.
    /// Falls back to the diff's added and context lines when full content is unavailable.
    pub fn new_lines(&self) -> Vec<(u32, &str)> {
        match &self.new_content {
            Some(content) => (1u32..).zip(content.lines()).collect(),
            None => self
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter_map(|line| match line {
                    DiffLine::Added { new_line, text } | DiffLine::Context { new_line, text } => {
                        Some((*new_line, text.as_str()))
                    }
                    DiffLine::Removed { .. } => None,
                })
                .collect(),
        }
    }

    /// Rough size in bytes of what a reviewer has to read for this file.
    pub fn review_weight(&self) -> usize {
        let diff: usize = self.hunks.iter().flat_map(|h| &h.lines).map(|l| l.text().len() + 1).sum();
        diff + self.new_content.as_ref().map_or(0, String::len)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub files: Vec<FileChange>,
}

impl ChangeSet {
    pub fn find(&self, path: &str) -> Option<&FileChange> {
        let wanted = normalize_path(path);
        self.files.iter().find(|f| f.path == wanted)
    }
}

fn normalize_path(path: &str) -> &str {
    let trimmed = path.trim();
    trimmed.strip_prefix("./").unwrap_or(trimmed)
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn added(new_line: u32, text: &str) -> DiffLine {
        DiffLine::Added { new_line, text: text.to_string() }
    }

    pub fn context(new_line: u32, text: &str) -> DiffLine {
        DiffLine::Context { new_line, text: text.to_string() }
    }

    pub fn removed(text: &str) -> DiffLine {
        DiffLine::Removed { text: text.to_string() }
    }

    pub fn file(path: &str, new_start: u32, lines: Vec<DiffLine>, content: Option<&str>) -> FileChange {
        FileChange {
            path: path.to_string(),
            status: ChangeStatus::Modified,
            is_binary: false,
            hunks: vec![Hunk { header: "@@".into(), new_start, lines }],
            new_content: content.map(str::to_string),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn changed_positions_include_added_lines_and_removal_points() {
        let f = file(
            "a.go",
            10,
            vec![context(10, "a"), removed("old"), added(11, "new"), context(12, "b"), removed("gone")],
            None,
        );
        assert_eq!(f.changed_positions(), vec![11, 13]);
    }

    #[test]
    fn is_near_change_respects_tolerance() {
        let f = file("a.go", 5, vec![added(5, "x")], None);
        assert!(f.is_near_change(8, 3));
        assert!(!f.is_near_change(9, 3));
    }

    #[test]
    fn new_lines_prefers_full_content() {
        let f = file("a.go", 2, vec![added(2, "b")], Some("a\nb\nc"));
        assert_eq!(f.new_lines(), vec![(1, "a"), (2, "b"), (3, "c")]);
    }

    #[test]
    fn new_lines_falls_back_to_diff_without_removed_lines() {
        let f = file("a.go", 1, vec![context(1, "a"), removed("x"), added(2, "b")], None);
        assert_eq!(f.new_lines(), vec![(1, "a"), (2, "b")]);
    }

    #[test]
    fn find_normalizes_dot_slash_prefix() {
        let set = ChangeSet { files: vec![file("src/a.rs", 1, vec![added(1, "x")], None)] };
        assert!(set.find("./src/a.rs").is_some());
        assert!(set.find("src/b.rs").is_none());
    }

    #[test]
    fn review_weight_counts_diff_and_content() {
        let f = file("a.py", 1, vec![added(1, "abc")], Some("abc"));
        assert_eq!(f.review_weight(), 4 + 3);
        assert!(f.has_content_change());
        assert_eq!(f.language(), Language::Python);
    }
}
