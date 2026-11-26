use serde::{Deserialize, Serialize};

use super::finding::{Category, Demotion, Finding, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackMark {
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FindingFeedback {
    pub dismissed: bool,
    pub cap: Option<Severity>,
    pub score: Option<u8>,
}

pub fn apply_feedback(findings: Vec<Finding>, feedback: &[FindingFeedback]) -> Vec<Finding> {
    findings
        .into_iter()
        .enumerate()
        .map(|(index, finding)| match feedback.get(index) {
            Some(entry) => apply_one(finding, entry),
            None => finding,
        })
        .collect()
}

fn apply_one(finding: Finding, entry: &FindingFeedback) -> Finding {
    let finding =
        if entry.dismissed { Finding { feedback: Some(FeedbackMark::Dismissed), ..finding } } else { finding };
    match entry.cap {
        Some(cap) if finding.category != Category::Security && finding.severity > cap => Finding {
            severity: cap,
            demoted: Some(Demotion::LowPrecision { score: entry.score.unwrap_or(0) }),
            ..finding
        },
        _ => finding,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::finding::fixtures::finding;

    fn capped(cap: Severity, score: u8) -> FindingFeedback {
        FindingFeedback { dismissed: false, cap: Some(cap), score: Some(score) }
    }

    #[test]
    fn dismissed_marks_the_finding() {
        let out = apply_feedback(
            vec![finding("a.go", 1, Severity::Major, "x")],
            &[FindingFeedback { dismissed: true, ..Default::default() }],
        );
        assert_eq!(out[0].feedback, Some(FeedbackMark::Dismissed));
        assert_eq!(out[0].severity, Severity::Major);
    }

    #[test]
    fn cap_lowers_severity_and_records_the_score() {
        let out = apply_feedback(vec![finding("a.go", 1, Severity::Critical, "x")], &[capped(Severity::Minor, 20)]);
        assert_eq!(out[0].severity, Severity::Minor);
        assert_eq!(out[0].demoted, Some(Demotion::LowPrecision { score: 20 }));
    }

    #[test]
    fn cap_that_does_not_lower_keeps_existing_demotion() {
        let before = Finding { demoted: Some(Demotion::MissingReproduce), ..finding("a.go", 1, Severity::Major, "x") };
        let out = apply_feedback(vec![before.clone()], &[capped(Severity::Major, 50)]);
        assert_eq!(out, vec![before]);
    }

    #[test]
    fn security_is_never_capped() {
        let before = Finding { category: Category::Security, ..finding("a.go", 1, Severity::Critical, "x") };
        let out = apply_feedback(vec![before.clone()], &[capped(Severity::Minor, 10)]);
        assert_eq!(out, vec![before]);
    }

    #[test]
    fn missing_score_defaults_to_zero_and_missing_entries_change_nothing() {
        let findings = vec![finding("a.go", 1, Severity::Major, "x"), finding("b.go", 2, Severity::Major, "y")];
        let entry = FindingFeedback { dismissed: false, cap: Some(Severity::Info), score: None };
        let out = apply_feedback(findings.clone(), &[entry]);
        assert_eq!(out[0].demoted, Some(Demotion::LowPrecision { score: 0 }));
        assert_eq!(out[1], findings[1]);
    }

    #[test]
    fn feedback_mark_round_trips_and_is_omitted_when_absent() {
        let plain = serde_json::to_value(finding("a.go", 1, Severity::Major, "x")).unwrap();
        assert!(plain.get("feedback").is_none());
        let marked = Finding { feedback: Some(FeedbackMark::Dismissed), ..finding("a.go", 1, Severity::Major, "x") };
        let value = serde_json::to_value(&marked).unwrap();
        assert_eq!(value["feedback"], "dismissed");
        assert_eq!(serde_json::from_value::<Finding>(value).unwrap(), marked);
    }
}
