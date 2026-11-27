use serde::Deserialize;
use serde_json::{Value, json};

use crate::review::domain::finding::{
    Category, Confidence, Finding, FindingSource, PreventionRule, Reproduce, Severity,
};
use crate::review::domain::requirements::{CriterionAssessment, CriterionStatus, EvidenceRef};
use crate::review::domain::unverifiable::UnverifiableReason;
use crate::review::domain::verification::{AuditVerdict, FindingAudit};

pub fn review_schema() -> Value {
    let finding = strict_object(json!({
        "file": string("Path exactly as given in the changed_file path attribute."),
        "line": integer("Line number in the NEW version of the file."),
        "severity": string_enum(Severity::ALL.map(Severity::as_str)),
        "category": string_enum(Category::LLM.map(Category::as_str)),
        "confidence": string_enum(Confidence::ALL.map(Confidence::as_str)),
        "title": string("One-line summary of the problem."),
        "explanation": string("What goes wrong, under which conditions, and the impact."),
        "evidence": string("1-3 lines copied verbatim from the new file, without line numbers or diff markers."),
        "suggestion": string("Concrete fix; empty string when there is no sensible one."),
        "prevention_rule": string(
            "Imperative, general project rule that would have prevented this class of defect anywhere in the \
             codebase (not this line); empty string when there is none."
        ),
        "rule_scope": string("Glob of the files the rule applies to, e.g. **/*.go; empty string for all files."),
        "reproduce": reproduce_schema(),
    }));
    strict_object(json!({ "findings": array(finding) }))
}

fn reproduce_schema() -> Value {
    strict_object(json!({
        "input": string(
            "Concrete trigger: the call, request, data or sequence of steps that makes the defect happen. \
             Required for critical; empty string otherwise if you have none."
        ),
        "expected": string("What correct code returns or does for that input."),
        "actual": string("What this code returns or does for that input, traced from the quoted code."),
        "command": string("Shell command or test invocation that demonstrates it; empty string when there is none."),
    }))
}

const UNCLEAR_REASONS: [&str; 3] = ["none", "ambiguous_requirement", "needs_context"];

pub fn assessment_schema() -> Value {
    let evidence = strict_object(json!({
        "file": string("Path exactly as given in the changed_file path attribute."),
        "line": integer("Line number in the NEW version of the file."),
        "quote": string("Code copied verbatim from an added or context line of the diff."),
    }));
    let assessment = strict_object(json!({
        "criterion_id": string("Id of the acceptance criterion, exactly as given."),
        "status": string_enum(CriterionStatus::ALL.map(CriterionStatus::as_str)),
        "explanation": string("Why this status, referring to the concrete change."),
        "evidence": array(evidence),
        "unclear_reason": json!({
            "type": "string",
            "enum": UNCLEAR_REASONS,
            "description": "Only for status unclear. ambiguous_requirement: the criterion itself is too vague to judge. \
                            needs_context: it depends on code or configuration not shown. none: for met and not_met."
        }),
    }));
    strict_object(json!({ "assessments": array(assessment) }))
}

pub fn audit_schema() -> Value {
    let audit = strict_object(json!({
        "index": integer("Zero-based position of the finding in the findings list, exactly as given."),
        "verdict": string_enum(["upheld", "contradicted", "unsupported"]),
        "counter_file": string(
            "Path (exactly as given) of the file whose code contradicts the finding; empty unless verdict is contradicted."
        ),
        "counter_line": integer("Line in the new file where that contradicting code is; 0 unless contradicted."),
        "counter_quote": string(
            "1-3 lines copied verbatim from the diff proving the change already handles what the finding claims is \
             missing/wrong; empty unless contradicted."
        ),
        "reason": string("One line: why this verdict."),
    }));
    strict_object(json!({ "audits": array(audit) }))
}

fn strict_object(properties: Value) -> Value {
    let required: Vec<&String> = properties.as_object().map(|p| p.keys().collect()).unwrap_or_default();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn integer(description: &str) -> Value {
    json!({ "type": "integer", "description": description })
}

fn string_enum<const N: usize>(values: [&str; N]) -> Value {
    let values: &[&str] = &values;
    json!({ "type": "string", "enum": values })
}

fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAnswer {
    findings: Vec<RawFinding>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFinding {
    file: String,
    line: i64,
    severity: Severity,
    category: Category,
    confidence: Confidence,
    title: String,
    explanation: String,
    evidence: String,
    suggestion: String,
    #[serde(default)]
    prevention_rule: String,
    #[serde(default)]
    rule_scope: String,
    #[serde(default)]
    reproduce: Option<RawReproduce>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReproduce {
    input: String,
    expected: String,
    actual: String,
    #[serde(default)]
    command: String,
}

impl RawReproduce {
    fn into_reproduce(self) -> Option<Reproduce> {
        Reproduce::new(&self.input, &self.expected, &self.actual, &self.command)
    }
}

impl ReviewAnswer {
    pub fn into_findings(self) -> Vec<Finding> {
        self.findings.into_iter().map(RawFinding::into_finding).collect()
    }
}

impl RawFinding {
    fn into_finding(self) -> Finding {
        Finding {
            file: self.file.trim().to_string(),
            line: to_line_number(self.line),
            severity: self.severity,
            category: self.category,
            confidence: self.confidence,
            title: self.title,
            explanation: self.explanation,
            evidence: self.evidence,
            suggestion: non_empty(self.suggestion),
            prevention_rule: PreventionRule::new(&self.prevention_rule, &self.rule_scope),
            source: FindingSource::Llm,
            reproduce: self.reproduce.and_then(RawReproduce::into_reproduce),
            demoted: None,
            feedback: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentAnswer {
    assessments: Vec<RawAssessment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAssessment {
    criterion_id: String,
    status: CriterionStatus,
    explanation: String,
    evidence: Vec<RawEvidence>,
    #[serde(default)]
    unclear_reason: String,
}

fn parse_unclear_reason(status: CriterionStatus, raw: &str) -> Option<UnverifiableReason> {
    if status != CriterionStatus::Unclear {
        return None;
    }
    match raw.trim() {
        "needs_context" => Some(UnverifiableReason::NeedsContext),
        _ => Some(UnverifiableReason::AmbiguousRequirement),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvidence {
    file: String,
    line: i64,
    quote: String,
}

impl AssessmentAnswer {
    pub fn into_assessments(self) -> Vec<CriterionAssessment> {
        self.assessments
            .into_iter()
            .map(|a| CriterionAssessment {
                criterion_id: a.criterion_id.trim().to_string(),
                reason: parse_unclear_reason(a.status, &a.unclear_reason),
                retried: false,
                human: None,
                status: a.status,
                explanation: a.explanation,
                evidence: a
                    .evidence
                    .into_iter()
                    .map(|e| EvidenceRef {
                        file: e.file.trim().to_string(),
                        line: to_line_number(e.line),
                        quote: e.quote,
                    })
                    .collect(),
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditAnswer {
    audits: Vec<AuditItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditItem {
    index: i64,
    verdict: String,
    counter_file: String,
    counter_line: i64,
    counter_quote: String,
    #[serde(default, rename = "reason")]
    _reason: String,
}

impl AuditAnswer {
    pub fn into_audits(self) -> Vec<FindingAudit> {
        self.audits.into_iter().filter_map(AuditItem::into_audit).collect()
    }
}

impl AuditItem {
    fn into_audit(self) -> Option<FindingAudit> {
        let verdict = match self.verdict.as_str() {
            "contradicted" => AuditVerdict::Contradicted,
            "unsupported" => AuditVerdict::Unsupported,
            _ => AuditVerdict::Upheld,
        };
        Some(FindingAudit {
            index: usize::try_from(self.index).ok()?,
            verdict,
            counter_file: self.counter_file.trim().to_string(),
            counter_line: to_line_number(self.counter_line),
            counter_quote: self.counter_quote,
        })
    }
}

fn to_line_number(raw: i64) -> u32 {
    u32::try_from(raw).unwrap_or(0)
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_strict(schema: &Value) {
        if schema["type"] == "object" {
            assert_eq!(schema["additionalProperties"], false);
            let mut props: Vec<_> = schema["properties"].as_object().unwrap().keys().cloned().collect();
            let mut required: Vec<_> =
                schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            props.sort();
            required.sort();
            assert_eq!(props, required);
            schema["properties"].as_object().unwrap().values().for_each(assert_strict);
        }
        if schema["type"] == "array" {
            assert_strict(&schema["items"]);
        }
    }

    #[test]
    fn schemas_follow_structured_output_rules() {
        assert_strict(&review_schema());
        assert_strict(&assessment_schema());
        assert_strict(&audit_schema());
    }

    #[test]
    fn enums_come_from_domain() {
        let schema = review_schema();
        let finding = &schema["properties"]["findings"]["items"]["properties"];
        assert_eq!(finding["severity"]["enum"], json!(["info", "minor", "major", "critical"]));
        assert_eq!(finding["category"]["enum"].as_array().unwrap().len(), Category::LLM.len());
        assert!(!finding["category"]["enum"].as_array().unwrap().contains(&json!("architecture")));
        let status = &assessment_schema()["properties"]["assessments"]["items"]["properties"]["status"];
        assert_eq!(status["enum"], json!(["met", "not_met", "unclear"]));
    }

    #[test]
    fn empty_suggestion_maps_to_none() {
        let answer: ReviewAnswer = serde_json::from_value(json!({"findings": [{
            "file": " a.py ", "line": 3, "severity": "major", "category": "data_loss", "confidence": "low",
            "title": "t", "explanation": "e", "evidence": "x = 1", "suggestion": "  "
        }]}))
        .unwrap();
        let findings = answer.into_findings();
        assert_eq!(findings[0].file, "a.py");
        assert_eq!(findings[0].suggestion, None);
        assert_eq!(findings[0].category, Category::DataLoss);
    }

    #[test]
    fn prevention_rule_is_parsed_and_optional() {
        let raw = |rule: &str, scope: &str| {
            let answer: ReviewAnswer = serde_json::from_value(json!({"findings": [{
                "file": "a.go", "line": 3, "severity": "major", "category": "error_handling", "confidence": "high",
                "title": "t", "explanation": "e", "evidence": "x", "suggestion": "",
                "prevention_rule": rule, "rule_scope": scope
            }]}))
            .unwrap();
            answer.into_findings().remove(0).prevention_rule
        };
        let rule = raw(" Always close sql.Rows with defer rows.Close(). ", "**/*.go").unwrap();
        assert_eq!(rule.text, "Always close sql.Rows with defer rows.Close().");
        assert_eq!(rule.scope.as_deref(), Some("**/*.go"));
        assert_eq!(raw("Check errors", "").unwrap().scope, None);
        assert_eq!(raw("", "**/*.go"), None);
        let schema = review_schema();
        let props = &schema["properties"]["findings"]["items"]["properties"];
        assert_eq!(props["prevention_rule"]["type"], "string");
        assert_eq!(props["rule_scope"]["type"], "string");
    }

    #[test]
    fn reproduce_is_parsed_and_invalid_reproduce_maps_to_none() {
        let raw = |reproduce: Value| {
            let answer: ReviewAnswer = serde_json::from_value(json!({"findings": [{
                "file": "a.go", "line": 3, "severity": "critical", "category": "correctness", "confidence": "high",
                "title": "t", "explanation": "e", "evidence": "x", "suggestion": "", "reproduce": reproduce
            }]}))
            .unwrap();
            answer.into_findings().remove(0).reproduce
        };
        let parsed =
            raw(json!({"input": "f(-1)", "expected": "error", "actual": "returns -1", "command": ""})).unwrap();
        assert_eq!((parsed.input.as_str(), parsed.command), ("f(-1)", None));
        assert_eq!(raw(json!({"input": "", "expected": "", "actual": "", "command": ""})), None);
        assert_eq!(raw(json!({"input": "f", "expected": "0", "actual": "0", "command": ""})), None);
        assert_eq!(raw(Value::Null), None);
        let schema = review_schema();
        let props = &schema["properties"]["findings"]["items"]["properties"]["reproduce"]["properties"];
        assert_eq!(props.as_object().unwrap().len(), 4);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let result: Result<ReviewAnswer, _> = serde_json::from_value(json!({"findings": [], "summary": "ok"}));
        assert!(result.is_err());
    }

    #[test]
    fn assessments_map_to_domain() {
        let answer: AssessmentAnswer = serde_json::from_value(json!({"assessments": [{
            "criterion_id": "AC-1", "status": "not_met", "explanation": "e",
            "evidence": [{"file": "a.py", "line": 2, "quote": "q"}], "unclear_reason": "none"
        }]}))
        .unwrap();
        let assessments = answer.into_assessments();
        assert_eq!(assessments[0].status, CriterionStatus::NotMet);
        assert_eq!(assessments[0].evidence[0], EvidenceRef { file: "a.py".into(), line: 2, quote: "q".into() });
    }

    #[test]
    fn unclear_reason_is_parsed_only_for_unclear_assessments() {
        let reason = |status: &str, raw: &str| {
            let answer: AssessmentAnswer = serde_json::from_value(json!({"assessments": [{
                "criterion_id": "AC-1", "status": status, "explanation": "e", "evidence": [], "unclear_reason": raw
            }]}))
            .unwrap();
            answer.into_assessments().remove(0).reason
        };
        assert_eq!(reason("unclear", "needs_context"), Some(UnverifiableReason::NeedsContext));
        assert_eq!(reason("unclear", "ambiguous_requirement"), Some(UnverifiableReason::AmbiguousRequirement));
        assert_eq!(reason("unclear", "none"), Some(UnverifiableReason::AmbiguousRequirement));
        assert_eq!(reason("unclear", "weird"), Some(UnverifiableReason::AmbiguousRequirement));
        assert_eq!(reason("met", "needs_context"), None);
        assert_eq!(reason("not_met", "none"), None);
        let props = &assessment_schema()["properties"]["assessments"]["items"]["properties"];
        assert_eq!(props["unclear_reason"]["enum"], json!(["none", "ambiguous_requirement", "needs_context"]));
    }

    #[test]
    fn out_of_range_line_numbers_do_not_fail_the_answer() {
        assert_eq!(to_line_number(-3), 0);
        assert_eq!(to_line_number(i64::from(u32::MAX) + 1), 0);
        assert_eq!(to_line_number(12), 12);
    }

    #[test]
    fn audit_answer_maps_verdicts_and_ignores_unknown_ones() {
        let item = |index: i64, verdict: &str| {
            json!({"index": index, "verdict": verdict, "counter_file": " a.py ", "counter_line": 4,
                   "counter_quote": "x = 1", "reason": "r"})
        };
        let answer: AuditAnswer = serde_json::from_value(json!({"audits": [
            item(0, "contradicted"), item(1, "unsupported"), item(2, "upheld"), item(3, "mystery"), item(-1, "contradicted")
        ]}))
        .unwrap();
        let audits = answer.into_audits();
        let verdicts: Vec<_> = audits.iter().map(|a| (a.index, a.verdict)).collect();
        assert_eq!(
            verdicts,
            vec![
                (0, AuditVerdict::Contradicted),
                (1, AuditVerdict::Unsupported),
                (2, AuditVerdict::Upheld),
                (3, AuditVerdict::Upheld)
            ]
        );
        assert_eq!((audits[0].counter_file.as_str(), audits[0].counter_line), ("a.py", 4));
    }
}
