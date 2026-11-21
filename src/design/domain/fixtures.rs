//! Test builders for the design domain.

use crate::review::domain::change_set::{ChangeStatus, DiffLine, FileChange, Hunk};

use super::spec::DesignSpec;

pub fn spec(texts: &[&str]) -> DesignSpec {
    DesignSpec {
        frame_name: "Checkout / Payment".into(),
        node_id: "12:345".into(),
        texts: texts.iter().map(|t| t.to_string()).collect(),
        ..DesignSpec::default()
    }
}

/// A new UI file whose lines are all added, numbered from 1.
pub fn ui_file(path: &str, lines: &[&str]) -> FileChange {
    file_with(path, &[], lines)
}

/// A modified file: `context` lines first (unchanged), then `added` lines.
pub fn file_with(path: &str, context: &[&str], added: &[&str]) -> FileChange {
    let mut diff = Vec::new();
    let mut number = 0;
    for text in context {
        number += 1;
        diff.push(DiffLine::Context { new_line: number, text: text.to_string() });
    }
    for text in added {
        number += 1;
        diff.push(DiffLine::Added { new_line: number, text: text.to_string() });
    }
    let content: Vec<&str> = context.iter().chain(added).copied().collect();
    FileChange {
        path: path.into(),
        status: ChangeStatus::Modified,
        is_binary: false,
        hunks: vec![Hunk { header: "@@".into(), new_start: 1, lines: diff }],
        new_content: Some(content.join("\n")),
    }
}
