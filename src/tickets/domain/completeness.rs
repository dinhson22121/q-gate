const MIN_DESCRIPTION_CHARS: usize = 60;

pub fn completeness_gaps(
    criteria_count: usize,
    description: &str,
    figma_count: usize,
    qa_source_count: usize,
) -> Vec<String> {
    let checks = [
        (criteria_count == 0, "no acceptance criteria"),
        (description.trim().chars().count() < MIN_DESCRIPTION_CHARS, "thin or missing description"),
        (figma_count == 0, "no Figma design link"),
        (qa_source_count == 0, "no linked QA ticket"),
    ];
    checks.into_iter().filter(|(is_gap, _)| *is_gap).map(|(_, gap)| gap.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn substantial() -> String {
        "x".repeat(MIN_DESCRIPTION_CHARS)
    }

    #[test]
    fn complete_ticket_has_no_gaps() {
        assert!(completeness_gaps(2, &substantial(), 1, 1).is_empty());
    }

    #[test]
    fn flags_missing_acceptance_criteria() {
        assert_eq!(completeness_gaps(0, &substantial(), 1, 1), vec!["no acceptance criteria"]);
    }

    #[test]
    fn flags_thin_description_after_trimming() {
        let padded = format!("   {}   ", "x".repeat(MIN_DESCRIPTION_CHARS - 1));
        assert_eq!(completeness_gaps(1, &padded, 1, 1), vec!["thin or missing description"]);
        assert_eq!(completeness_gaps(1, "", 1, 1), vec!["thin or missing description"]);
    }

    #[test]
    fn flags_missing_figma_link() {
        assert_eq!(completeness_gaps(1, &substantial(), 0, 1), vec!["no Figma design link"]);
    }

    #[test]
    fn flags_missing_qa_ticket() {
        assert_eq!(completeness_gaps(1, &substantial(), 1, 0), vec!["no linked QA ticket"]);
    }

    #[test]
    fn reports_every_gap_in_order() {
        assert_eq!(completeness_gaps(0, "", 0, 0).len(), 4);
    }
}
