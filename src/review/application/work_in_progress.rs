pub fn subject_marks_in_progress(message: &str) -> bool {
    message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|subject| subject.to_ascii_lowercase().starts_with("[wip]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wip_subject_is_detected_case_insensitively_with_leading_whitespace() {
        assert!(subject_marks_in_progress("[WIP] add cart"));
        assert!(subject_marks_in_progress("  [wip] add cart"));
        assert!(subject_marks_in_progress("\n\n[Wip] after blank lines"));
        assert!(subject_marks_in_progress("[WIP]"));
        assert!(!subject_marks_in_progress("feat: [WIP] not at the start"));
        assert!(!subject_marks_in_progress("add cart\n[WIP] in the body"));
        assert!(!subject_marks_in_progress("WIP: no brackets"));
        assert!(!subject_marks_in_progress(""));
    }
}
