use std::collections::HashSet;

use serde::Serialize;

use super::change_set::{ChangeSet, FileChange};
use super::finding::{Confidence, Finding};

pub const LINE_TOLERANCE: u32 = 3;
pub const EVIDENCE_WINDOW: u32 = 12;
pub const MIN_EVIDENCE_CHARS: usize = 8;
const ELLIPSES: [&str; 2] = ["...", "…"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    UnknownFile,
    LineOutsideChange,
    MissingEvidence,
    TrivialEvidence,
    EvidenceNotFound,
    ContradictedByCode,
    Unsupported,
}

impl RejectReason {
    pub fn describe(self) -> &'static str {
        match self {
            Self::UnknownFile => "file is not part of the change",
            Self::LineOutsideChange => "line is not near any changed code",
            Self::MissingEvidence => "no code quoted as evidence",
            Self::TrivialEvidence => "quoted code is too short to identify a location",
            Self::EvidenceNotFound => "quoted code does not exist near the reported line",
            Self::ContradictedByCode => "the change already does what the finding says is missing",
            Self::Unsupported => "the finding's claim is not supported by the quoted code",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectedFinding {
    pub finding: Finding,
    pub reason: RejectReason,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verification {
    pub accepted: Vec<Finding>,
    pub rejected: Vec<RejectedFinding>,
}

pub fn verify_findings(changes: &ChangeSet, findings: Vec<Finding>) -> Verification {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for finding in findings {
        match check(changes, &finding) {
            Ok(()) => accepted.push(finding),
            Err(reason) => rejected.push(RejectedFinding { finding, reason }),
        }
    }
    Verification { accepted: deduplicate(accepted), rejected }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditVerdict {
    Upheld,
    Contradicted,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingAudit {
    pub index: usize,
    pub verdict: AuditVerdict,
    pub counter_file: String,
    pub counter_line: u32,
    pub counter_quote: String,
}

pub fn apply_audit(changes: &ChangeSet, verification: Verification, audits: Vec<FindingAudit>) -> Verification {
    let Verification { accepted, mut rejected } = verification;
    let mut survivors = Vec::new();
    for (index, finding) in accepted.into_iter().enumerate() {
        let verdicts: Vec<&FindingAudit> = audits.iter().filter(|a| a.index == index).collect();
        let refuted = verdicts.iter().any(|a| {
            a.verdict == AuditVerdict::Contradicted
                && evidence_exists(changes, &a.counter_file, a.counter_line, &a.counter_quote)
        });
        if refuted {
            rejected.push(RejectedFinding { finding, reason: RejectReason::ContradictedByCode });
            continue;
        }
        let unsupported = verdicts.iter().any(|a| a.verdict == AuditVerdict::Unsupported);
        survivors.push(if unsupported { Finding { confidence: Confidence::Low, ..finding } } else { finding });
    }
    Verification { accepted: deduplicate(survivors), rejected }
}

fn check(changes: &ChangeSet, finding: &Finding) -> Result<(), RejectReason> {
    let file = changes.find(&finding.file).ok_or(RejectReason::UnknownFile)?;
    if !file.is_near_change(finding.line, LINE_TOLERANCE) {
        return Err(RejectReason::LineOutsideChange);
    }
    if evidence_candidates(&finding.evidence).is_empty() {
        return Err(RejectReason::MissingEvidence);
    }
    if !is_substantial(&finding.evidence) {
        return Err(RejectReason::TrivialEvidence);
    }
    if quote_near_line(file, finding.line, &finding.evidence) { Ok(()) } else { Err(RejectReason::EvidenceNotFound) }
}

pub fn evidence_exists(changes: &ChangeSet, path: &str, line: u32, quote: &str) -> bool {
    changes.find(path).is_some_and(|file| {
        file.is_near_change(line, LINE_TOLERANCE) && is_substantial(quote) && quote_near_line(file, line, quote)
    })
}

fn is_substantial(quote: &str) -> bool {
    quote.chars().filter(|c| !c.is_whitespace()).count() >= MIN_EVIDENCE_CHARS
}

fn quote_near_line(file: &FileChange, line: u32, quote: &str) -> bool {
    let candidates = evidence_candidates(quote);
    if candidates.is_empty() {
        return false;
    }
    let mut window: Vec<&str> = file
        .new_lines()
        .into_iter()
        .filter(|(number, _)| number.abs_diff(line) <= EVIDENCE_WINDOW)
        .map(|(_, text)| text)
        .collect();
    let new_code = normalize_whitespace(&window.join("\n"));
    window = file.removed_lines_near(line, EVIDENCE_WINDOW);
    let removed_code = normalize_whitespace(&window.join("\n"));
    candidates.iter().any(|c| contains_in_order(&new_code, c) || contains_in_order(&removed_code, c))
}

fn contains_in_order(haystack: &str, quote: &str) -> bool {
    let mut rest = haystack;
    let mut segments = vec![quote];
    for ellipsis in ELLIPSES {
        segments = segments.iter().flat_map(|s| s.split(ellipsis)).collect();
    }
    for segment in segments.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        match rest.find(segment) {
            Some(position) => rest = &rest[position + segment.len()..],
            None => return false,
        }
    }
    true
}

fn evidence_candidates(raw: &str) -> Vec<String> {
    let mut candidates: Vec<String> = [normalize_whitespace(raw), normalize_whitespace(&strip_line_prefixes(raw))]
        .into_iter()
        .filter(|c| !c.is_empty())
        .collect();
    candidates.dedup();
    candidates
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_line_prefixes(raw: &str) -> String {
    raw.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let without_number = trimmed
                .split_once('|')
                .filter(|(prefix, _)| {
                    let p = prefix.trim();
                    !p.is_empty() && p.trim_end_matches(['+', '-', ' ']).chars().all(|c| c.is_ascii_digit())
                })
                .map_or(trimmed, |(_, rest)| rest);
            without_number.strip_prefix(['+', '-']).unwrap_or(without_number)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn deduplicate(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by(|a, b| {
        b.severity.cmp(&a.severity).then_with(|| a.file.cmp(&b.file)).then_with(|| a.line.cmp(&b.line))
    });
    let mut seen = HashSet::new();
    findings.retain(|f| seen.insert((f.file.clone(), f.line, f.category)));
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::fixtures::{added, context, file};
    use crate::review::domain::finding::Severity;
    use crate::review::domain::finding::fixtures::finding;

    fn changes() -> ChangeSet {
        let content =
            "package main\n\nfunc get(id string) {\n\tq := \"SELECT * FROM u WHERE id=\" + id\n\tdb.Query(q)\n}\n";
        ChangeSet {
            files: vec![file(
                "main.go",
                3,
                vec![
                    context(3, "func get(id string) {"),
                    added(4, "\tq := \"SELECT * FROM u WHERE id=\" + id"),
                    context(5, "\tdb.Query(q)"),
                ],
                Some(content),
            )],
        }
    }

    #[test]
    fn accepts_finding_with_matching_evidence_on_changed_line() {
        let f = finding("main.go", 4, Severity::Critical, "q := \"SELECT * FROM u WHERE id=\" + id");
        let result = verify_findings(&changes(), vec![f]);
        assert_eq!(result.accepted.len(), 1);
        assert!(result.rejected.is_empty());
    }

    #[test]
    fn accepts_evidence_copied_with_line_numbers_or_diff_markers() {
        let numbered = finding("main.go", 4, Severity::Major, "   4 | \tq := \"SELECT * FROM u WHERE id=\" + id");
        let diffed = finding("./main.go", 5, Severity::Minor, "+\tq := \"SELECT * FROM u WHERE id=\" + id");
        let result = verify_findings(&changes(), vec![numbered, diffed]);
        assert_eq!(result.accepted.len(), 2);
    }

    #[test]
    fn rejects_unknown_file() {
        let result = verify_findings(&changes(), vec![finding("other.go", 4, Severity::Major, "x")]);
        assert_eq!(result.rejected[0].reason, RejectReason::UnknownFile);
    }

    #[test]
    fn rejects_line_far_from_change() {
        let result = verify_findings(&changes(), vec![finding("main.go", 40, Severity::Major, "package main")]);
        assert_eq!(result.rejected[0].reason, RejectReason::LineOutsideChange);
    }

    #[test]
    fn rejects_missing_or_hallucinated_evidence() {
        let empty = finding("main.go", 4, Severity::Major, "   ");
        let invented = finding("main.go", 4, Severity::Major, "os.RemoveAll(path)");
        let result = verify_findings(&changes(), vec![empty, invented]);
        let reasons: Vec<_> = result.rejected.iter().map(|r| r.reason).collect();
        assert_eq!(reasons, vec![RejectReason::MissingEvidence, RejectReason::EvidenceNotFound]);
        assert!(result.accepted.is_empty());
    }

    #[test]
    fn deduplicates_by_location_and_category_keeping_most_severe() {
        let minor = finding("main.go", 4, Severity::Minor, "db.Query(q)");
        let critical = finding("main.go", 4, Severity::Critical, "db.Query(q)");
        let other_line = finding("main.go", 5, Severity::Major, "db.Query(q)");
        let result = verify_findings(&changes(), vec![minor, critical, other_line]);
        let severities: Vec<_> = result.accepted.iter().map(|f| f.severity).collect();
        assert_eq!(severities, vec![Severity::Critical, Severity::Major]);
    }

    #[test]
    fn reject_reasons_have_descriptions() {
        assert!(!RejectReason::EvidenceNotFound.describe().is_empty());
        assert!(!RejectReason::UnknownFile.describe().is_empty());
        assert!(!RejectReason::LineOutsideChange.describe().is_empty());
        assert!(!RejectReason::MissingEvidence.describe().is_empty());
        assert!(!RejectReason::ContradictedByCode.describe().is_empty());
        assert!(!RejectReason::Unsupported.describe().is_empty());
    }

    #[test]
    fn rejects_trivial_quotes_that_match_anywhere() {
        let result = verify_findings(&changes(), vec![finding("main.go", 4, Severity::Major, "}")]);
        assert_eq!(result.rejected[0].reason, RejectReason::TrivialEvidence);
    }

    #[test]
    fn accepts_quotes_with_ellipsis_when_parts_appear_in_order() {
        let elided = finding("main.go", 4, Severity::Major, "q := \"SELECT ... + id");
        let reversed = finding("main.go", 5, Severity::Major, "db.Query(q) … q := \"SELECT");
        let result = verify_findings(&changes(), vec![elided, reversed]);
        assert_eq!(result.accepted.len(), 1);
        assert_eq!(result.rejected[0].reason, RejectReason::EvidenceNotFound);
    }

    #[test]
    fn accepts_evidence_quoting_removed_code_near_the_removal() {
        use crate::review::domain::change_set::fixtures::removed;
        let set = ChangeSet {
            files: vec![file(
                "auth.go",
                10,
                vec![
                    context(10, "func login(u User) {"),
                    removed("\tif !u.Verified { return ErrUnverified }"),
                    context(11, "\treturn issue(u)"),
                ],
                Some("func login(u User) {\n\treturn issue(u)\n"),
            )],
        };
        let f = finding("auth.go", 11, Severity::Critical, "if !u.Verified { return ErrUnverified }");
        assert_eq!(verify_findings(&set, vec![f]).accepted.len(), 1);
        assert!(evidence_exists(&set, "auth.go", 11, "if !u.Verified { return ErrUnverified }"));
        assert!(!evidence_exists(&set, "auth.go", 11, "}"));
    }

    fn audit(index: usize, verdict: AuditVerdict, file: &str, line: u32, quote: &str) -> FindingAudit {
        FindingAudit { index, verdict, counter_file: file.into(), counter_line: line, counter_quote: quote.into() }
    }

    fn two_accepted() -> Verification {
        let first = finding("main.go", 4, Severity::Critical, "q := \"SELECT * FROM u WHERE id=\" + id");
        let second = finding("main.go", 5, Severity::Major, "db.Query(q)");
        verify_findings(&changes(), vec![first, second])
    }

    #[test]
    fn contradicted_with_grounded_counter_evidence_moves_finding_to_rejected() {
        let verification = two_accepted();
        let audits = vec![audit(1, AuditVerdict::Contradicted, "main.go", 5, "db.Query(q)")];
        let result = apply_audit(&changes(), verification, audits);
        assert_eq!(result.accepted.len(), 1);
        assert_eq!(result.accepted[0].line, 4);
        assert_eq!(result.rejected.len(), 1);
        assert_eq!(result.rejected[0].reason, RejectReason::ContradictedByCode);
        assert_eq!(result.rejected[0].finding.line, 5);
    }

    #[test]
    fn contradicted_with_ungrounded_counter_evidence_keeps_finding() {
        let audits = vec![audit(0, AuditVerdict::Contradicted, "main.go", 4, "sanitize(id)")];
        let result = apply_audit(&changes(), two_accepted(), audits);
        assert_eq!(result.accepted.len(), 2);
        assert!(result.rejected.is_empty());
    }

    #[test]
    fn unsupported_keeps_finding_with_low_confidence() {
        let audits = vec![audit(0, AuditVerdict::Unsupported, "", 0, "")];
        let result = apply_audit(&changes(), two_accepted(), audits);
        assert_eq!(result.accepted.len(), 2);
        assert_eq!(result.accepted[0].confidence, Confidence::Low);
        assert_ne!(result.accepted[1].confidence, Confidence::Low);
        assert!(result.rejected.is_empty());
    }

    #[test]
    fn upheld_and_missing_audits_leave_findings_unchanged() {
        let before = two_accepted();
        let audits = vec![audit(0, AuditVerdict::Upheld, "", 0, "")];
        let result = apply_audit(&changes(), before.clone(), audits);
        assert_eq!(result, before);
    }

    #[test]
    fn out_of_range_index_is_ignored() {
        let before = two_accepted();
        let audits = vec![audit(9, AuditVerdict::Contradicted, "main.go", 5, "db.Query(q)")];
        assert_eq!(apply_audit(&changes(), before.clone(), audits), before);
    }

    #[test]
    fn previous_rejections_are_preserved() {
        let mut before = two_accepted();
        before.rejected.push(RejectedFinding {
            finding: finding("main.go", 4, Severity::Minor, "x"),
            reason: RejectReason::EvidenceNotFound,
        });
        let audits = vec![audit(1, AuditVerdict::Contradicted, "main.go", 5, "db.Query(q)")];
        let result = apply_audit(&changes(), before, audits);
        let reasons: Vec<_> = result.rejected.iter().map(|r| r.reason).collect();
        assert_eq!(reasons, vec![RejectReason::EvidenceNotFound, RejectReason::ContradictedByCode]);
    }
}
