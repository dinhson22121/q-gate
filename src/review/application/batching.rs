use std::ops::Range;

use crate::review::domain::change_set::FileChange;

pub fn plan_batches(files: &[FileChange], max_weight: usize, max_files: usize) -> Vec<Range<usize>> {
    let max_files = max_files.max(1);
    let mut batches = Vec::new();
    let mut start = 0;
    let mut weight = 0;
    for (index, file) in files.iter().enumerate() {
        let file_weight = file.review_weight();
        let count = index - start;
        if count > 0 && (weight + file_weight > max_weight || count >= max_files) {
            batches.push(start..index);
            start = index;
            weight = 0;
        }
        weight += file_weight;
    }
    if start < files.len() {
        batches.push(start..files.len());
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::fixtures::{added, file};

    fn sized(path: &str, size: usize) -> FileChange {
        file(path, 1, vec![added(1, "x")], Some(&"y".repeat(size)))
    }

    #[test]
    fn empty_input_has_no_batches() {
        assert!(plan_batches(&[], 100, 5).is_empty());
    }

    #[test]
    fn splits_on_weight() {
        let files = vec![sized("a", 50), sized("b", 50), sized("c", 50)];
        assert_eq!(plan_batches(&files, 110, 10), vec![0..2, 2..3]);
    }

    #[test]
    fn splits_on_file_count() {
        let files = vec![sized("a", 1), sized("b", 1), sized("c", 1)];
        assert_eq!(plan_batches(&files, 1000, 2), vec![0..2, 2..3]);
    }

    #[test]
    fn oversized_file_gets_own_batch() {
        let files = vec![sized("a", 10), sized("big", 500), sized("c", 10)];
        assert_eq!(plan_batches(&files, 100, 10), vec![0..1, 1..2, 2..3]);
    }
}
