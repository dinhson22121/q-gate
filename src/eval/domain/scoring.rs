use serde::{Deserialize, Serialize};

use super::case::{ExpectedFinding, ExpectedStatus, ForbiddenFinding, GoldenCase};
use super::stats::{Counts, CriterionCounts};
use crate::review::application::{Category, CriterionStatus, Severity};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedFinding {
    pub file: String,
    pub line: u32,
    pub category: Category,
    pub severity: Severity,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunMeasure {
    pub repetition: u32,
    pub duration_ms: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedOutcome {
    pub label: String,
    pub category: Category,
    pub caught: bool,
    pub matched_line: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionVerdict {
    Correct,
    Incorrect,
    Unclear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionOutcome {
    pub index: usize,
    pub text: String,
    pub expect: ExpectedStatus,
    pub actual: CriterionStatus,
    pub verdict: CriterionVerdict,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseScore {
    pub case_id: String,
    pub repetition: u32,
    pub expected: Vec<ExpectedOutcome>,
    pub forbidden_hits: Vec<ObservedFinding>,
    pub unlabeled: Vec<ObservedFinding>,
    pub criteria: Vec<CriterionOutcome>,
    pub duration_ms: u64,
    pub cost_usd: f64,
}

impl CaseScore {
    pub fn counts(&self) -> Counts {
        let tp = self.expected.iter().filter(|e| e.caught).count() as u32;
        Counts {
            tp,
            missed: self.expected.len() as u32 - tp,
            fp: self.forbidden_hits.len() as u32,
            unlabeled: self.unlabeled.len() as u32,
        }
    }

    pub fn criterion_counts(&self) -> CriterionCounts {
        let count = |verdict| self.criteria.iter().filter(|c| c.verdict == verdict).count() as u32;
        CriterionCounts {
            total: self.criteria.len() as u32,
            correct: count(CriterionVerdict::Correct),
            incorrect: count(CriterionVerdict::Incorrect),
            unclear: count(CriterionVerdict::Unclear),
        }
    }
}

pub fn score_case(
    case: &GoldenCase,
    findings: &[ObservedFinding],
    statuses: &[Option<CriterionStatus>],
    measure: RunMeasure,
) -> CaseScore {
    let assignment = assign_expected(&case.expected, findings);
    let mut used = vec![false; findings.len()];
    let expected = case
        .expected
        .iter()
        .zip(&assignment)
        .map(|(expected, matched)| {
            if let Some(index) = matched {
                used[*index] = true;
            }
            ExpectedOutcome {
                label: expected.label(),
                category: expected.category,
                caught: matched.is_some(),
                matched_line: matched.map(|index| findings[index].line),
            }
        })
        .collect();
    let (mut forbidden_hits, mut unlabeled) = (Vec::new(), Vec::new());
    for (finding, _) in findings.iter().zip(&used).filter(|(_, used)| !**used) {
        if case.forbidden.iter().any(|forbidden| hits_forbidden(forbidden, finding)) {
            forbidden_hits.push(finding.clone());
        } else {
            unlabeled.push(finding.clone());
        }
    }
    CaseScore {
        case_id: case.id.clone(),
        repetition: measure.repetition,
        expected,
        forbidden_hits,
        unlabeled,
        criteria: score_criteria(case, statuses),
        duration_ms: measure.duration_ms,
        cost_usd: measure.cost_usd,
    }
}

fn assign_expected(expected: &[ExpectedFinding], findings: &[ObservedFinding]) -> Vec<Option<usize>> {
    let mut candidates: Vec<(u32, usize, usize)> = expected
        .iter()
        .enumerate()
        .flat_map(|(e, expectation)| {
            findings
                .iter()
                .enumerate()
                .filter(|(_, finding)| matches_expected(expectation, finding))
                .map(move |(f, finding)| (finding.line.abs_diff(expectation.line), e, f))
        })
        .collect();
    candidates.sort_unstable();
    let mut assignment = vec![None; expected.len()];
    let mut taken = vec![false; findings.len()];
    for (_, e, f) in candidates {
        if assignment[e].is_none() && !taken[f] {
            assignment[e] = Some(f);
            taken[f] = true;
        }
    }
    assignment
}

fn matches_expected(expected: &ExpectedFinding, finding: &ObservedFinding) -> bool {
    finding.file == expected.file
        && finding.category == expected.category
        && finding.line.abs_diff(expected.line) <= expected.line_tolerance
        && expected.min_severity.is_none_or(|minimum| finding.severity >= minimum)
}

fn hits_forbidden(forbidden: &ForbiddenFinding, finding: &ObservedFinding) -> bool {
    finding.file == forbidden.file
        && finding.line.abs_diff(forbidden.line) <= forbidden.line_tolerance
        && forbidden.category.is_none_or(|category| category == finding.category)
        && forbidden.min_severity.is_none_or(|minimum| finding.severity >= minimum)
}

fn score_criteria(case: &GoldenCase, statuses: &[Option<CriterionStatus>]) -> Vec<CriterionOutcome> {
    case.criteria
        .iter()
        .enumerate()
        .filter_map(|(index, criterion)| {
            let expect = criterion.expect?;
            let actual = statuses.get(index).copied().flatten().unwrap_or(CriterionStatus::Unclear);
            let verdict = match actual {
                CriterionStatus::Unclear => CriterionVerdict::Unclear,
                status if expect.matches(status) => CriterionVerdict::Correct,
                _ => CriterionVerdict::Incorrect,
            };
            Some(CriterionOutcome { index, text: criterion.text.clone(), expect, actual, verdict })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::domain::fixtures::{case_with, finding};

    const MEASURE: RunMeasure = RunMeasure { repetition: 0, duration_ms: 10, cost_usd: 0.5 };

    fn expected(file: &str, line: u32, min_severity: Option<Severity>) -> ExpectedFinding {
        ExpectedFinding { file: file.into(), line, line_tolerance: 3, category: Category::Security, min_severity }
    }

    fn forbidden(file: &str, line: u32, category: Option<Category>) -> ForbiddenFinding {
        ForbiddenFinding { file: file.into(), line, line_tolerance: 3, category, min_severity: None }
    }

    #[test]
    fn a_finding_within_tolerance_catches_the_expected_one() {
        let case = case_with(vec![expected("a.go", 10, None)], vec![]);

        let score = score_case(&case, &[finding("a.go", 13, Category::Security, Severity::Major)], &[], MEASURE);

        assert_eq!(score.counts(), Counts { tp: 1, missed: 0, fp: 0, unlabeled: 0 });
        assert_eq!(score.expected[0].matched_line, Some(13));
    }

    #[test]
    fn wrong_file_category_line_or_severity_misses() {
        let case = case_with(vec![expected("a.go", 10, Some(Severity::Major))], vec![]);
        let misses = [
            finding("b.go", 10, Category::Security, Severity::Major),
            finding("a.go", 10, Category::Correctness, Severity::Major),
            finding("a.go", 14, Category::Security, Severity::Major),
            finding("a.go", 10, Category::Security, Severity::Minor),
        ];

        let score = score_case(&case, &misses, &[], MEASURE);

        assert_eq!(score.counts(), Counts { tp: 0, missed: 1, fp: 0, unlabeled: 4 });
    }

    #[test]
    fn each_expected_is_matched_at_most_once_by_the_closest_finding() {
        let case = case_with(vec![expected("a.go", 10, None), expected("a.go", 14, None)], vec![]);
        let findings = [
            finding("a.go", 12, Category::Security, Severity::Major),
            finding("a.go", 13, Category::Security, Severity::Major),
        ];

        let score = score_case(&case, &findings, &[], MEASURE);

        assert_eq!(score.expected[0].matched_line, Some(12));
        assert_eq!(score.expected[1].matched_line, Some(13));
        assert_eq!(score.counts().unlabeled, 0);
    }

    #[test]
    fn one_finding_cannot_catch_two_expectations() {
        let case = case_with(vec![expected("a.go", 10, None), expected("a.go", 11, None)], vec![]);

        let score = score_case(&case, &[finding("a.go", 10, Category::Security, Severity::Major)], &[], MEASURE);

        assert_eq!(score.counts(), Counts { tp: 1, missed: 1, fp: 0, unlabeled: 0 });
        assert!(score.expected[0].caught && !score.expected[1].caught);
    }

    #[test]
    fn a_forbidden_entry_with_a_minimum_severity_ignores_lesser_findings() {
        let entry = ForbiddenFinding { min_severity: Some(Severity::Major), ..forbidden("a.go", 10, None) };
        let case = case_with(vec![], vec![entry]);
        let findings = [
            finding("a.go", 10, Category::Security, Severity::Minor),
            finding("a.go", 11, Category::Security, Severity::Critical),
        ];

        let scored = score_case(&case, &findings, &[], MEASURE);

        assert_eq!(scored.forbidden_hits.iter().map(|f| f.line).collect::<Vec<_>>(), vec![11]);
        assert_eq!(scored.unlabeled.iter().map(|f| f.line).collect::<Vec<_>>(), vec![10]);
    }

    #[test]
    fn forbidden_hits_are_false_positives_and_other_findings_are_unlabeled() {
        let case = case_with(vec![], vec![forbidden("a.go", 10, Some(Category::Security)), forbidden("b.go", 1, None)]);
        let findings = [
            finding("a.go", 11, Category::Security, Severity::Critical),
            finding("a.go", 11, Category::Performance, Severity::Minor),
            finding("b.go", 2, Category::Maintainability, Severity::Info),
            finding("c.go", 1, Category::Security, Severity::Major),
        ];

        let score = score_case(&case, &findings, &[], MEASURE);

        assert_eq!(score.counts(), Counts { tp: 0, missed: 0, fp: 2, unlabeled: 2 });
        assert_eq!(score.forbidden_hits[0].file, "a.go");
        assert_eq!(score.unlabeled[1].file, "c.go");
    }

    #[test]
    fn a_finding_matching_both_expected_and_forbidden_counts_as_caught() {
        let case = case_with(vec![expected("a.go", 10, None)], vec![forbidden("a.go", 10, None)]);

        let score = score_case(&case, &[finding("a.go", 10, Category::Security, Severity::Major)], &[], MEASURE);

        assert_eq!(score.counts(), Counts { tp: 1, missed: 0, fp: 0, unlabeled: 0 });
    }

    #[test]
    fn criteria_are_correct_incorrect_or_unclear() {
        let case = crate::eval::domain::fixtures::case_with_criteria(&[
            Some(ExpectedStatus::Met),
            Some(ExpectedStatus::Met),
            Some(ExpectedStatus::NotMet),
            Some(ExpectedStatus::Met),
            None,
        ]);
        let statuses = [
            Some(CriterionStatus::Met),
            Some(CriterionStatus::NotMet),
            Some(CriterionStatus::NotMet),
            None,
            Some(CriterionStatus::Met),
        ];

        let score = score_case(&case, &[], &statuses, MEASURE);

        let verdicts: Vec<_> = score.criteria.iter().map(|c| (c.index, c.verdict)).collect();
        assert_eq!(
            verdicts,
            [
                (0, CriterionVerdict::Correct),
                (1, CriterionVerdict::Incorrect),
                (2, CriterionVerdict::Correct),
                (3, CriterionVerdict::Unclear),
            ]
        );
        assert_eq!(score.criterion_counts(), CriterionCounts { total: 4, correct: 2, incorrect: 1, unclear: 1 });
    }

    #[test]
    fn measures_are_carried_onto_the_score() {
        let case = case_with(vec![expected("a.go", 1, None)], vec![]);

        let score = score_case(&case, &[], &[], RunMeasure { repetition: 2, duration_ms: 99, cost_usd: 1.25 });

        assert_eq!((score.repetition, score.duration_ms, score.cost_usd), (2, 99, 1.25));
        assert_eq!(score.case_id, "case");
    }
}
