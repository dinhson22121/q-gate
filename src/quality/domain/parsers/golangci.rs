//! golangci-lint with only depguard enabled: `golangci-lint run --output.json.path=stdout ...`.

use std::path::Path;

use serde_json::Value;

use crate::quality::domain::boundary::ImportBoundary;
use crate::quality::domain::finding::{QualityCategory, QualityFinding, QualitySeverity, QualityTool};
use crate::quality::domain::{architecture_rule, json_documents, relative_to};

const DEPGUARD: &str = "depguard";
const LIST_MARKER: &str = "from list '";

/// The depguard list name is the boundary id (depguard lower-cases it), which ties an issue back to its rule.
pub fn parse(output: &str, root: &Path, boundaries: &[ImportBoundary]) -> Option<Vec<QualityFinding>> {
    let report = json_documents(output).into_iter().find(|doc| doc.get("Issues").is_some())?;
    let issues = report["Issues"].as_array().map(Vec::as_slice).unwrap_or_default();
    Some(issues.iter().filter_map(|issue| finding(issue, root, boundaries)).collect())
}

fn finding(issue: &Value, root: &Path, boundaries: &[ImportBoundary]) -> Option<QualityFinding> {
    if issue["FromLinter"].as_str()? != DEPGUARD {
        return None;
    }
    let text = issue["Text"].as_str()?;
    let file = relative_to(issue["Pos"]["Filename"].as_str()?, root);
    let line = u32::try_from(issue["Pos"]["Line"].as_u64()?).ok()?;
    let boundary = list_name(text).and_then(|name| boundaries.iter().find(|b| b.id.eq_ignore_ascii_case(name)));
    let evidence = issue["SourceLines"][0].as_str().map_or_else(|| text.to_string(), |s| s.trim().to_string());
    let (identifier, approved_rule, rule) = match boundary {
        Some(b) => (b.id.clone(), b.approved, architecture_rule(b)),
        None => {
            (DEPGUARD.to_string(), false, crate::quality::domain::QualityRule { text: text.to_string(), scope: None })
        }
    };
    Some(QualityFinding {
        tool: QualityTool::Depguard,
        category: QualityCategory::Architecture,
        severity: QualitySeverity::Medium,
        file,
        line,
        identifier,
        aliases: vec![],
        title: text.to_string(),
        evidence,
        is_new: false,
        approved_rule,
        dependency: None,
        prevention_rule: rule,
    })
}

fn list_name(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once(LIST_MARKER)?;
    rest.split_once('\'').map(|(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = include_str!("../../fixtures/golangci.json");

    fn boundary(approved: bool) -> ImportBoundary {
        ImportBoundary {
            id: "domain-purity".into(),
            title: "Pure domain".into(),
            scope_globs: vec!["internal/domain/**".into()],
            deny: vec!["example.com/app/internal/infra".into()],
            approved,
        }
    }

    #[test]
    fn parses_a_real_depguard_report() {
        let findings = parse(REPORT, Path::new("/snap"), &[boundary(true)]).unwrap();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!((f.file.as_str(), f.line, f.identifier.as_str()), ("internal/domain/user.go", 4, "domain-purity"));
        assert_eq!(f.evidence, "\"example.com/app/internal/infra\"");
        assert!(f.approved_rule);
        assert_eq!(f.category, QualityCategory::Architecture);
        assert_eq!(f.prevention_rule.scope.as_deref(), Some("**/internal/domain/**"));
        assert!(f.prevention_rule.text.contains("must not import example.com/app/internal/infra"));
    }

    #[test]
    fn depguard_lowercases_list_names() {
        let mut b = boundary(true);
        b.id = "Domain-Purity".into();
        assert_eq!(parse(REPORT, Path::new("/snap"), &[b]).unwrap()[0].identifier, "Domain-Purity");
    }

    #[test]
    fn proposed_rules_are_not_approved() {
        let f = &parse(REPORT, Path::new("/snap"), &[boundary(false)]).unwrap()[0];
        assert!(!f.approved_rule);
        assert!(!f.is_blocking());
    }

    #[test]
    fn unknown_list_and_other_linters_are_handled() {
        let f = &parse(REPORT, Path::new("/snap"), &[]).unwrap()[0];
        assert_eq!(f.identifier, "depguard");
        assert!(!f.approved_rule);
        let other = r#"{"Issues":[{"FromLinter":"govet","Text":"x","Pos":{"Filename":"a.go","Line":1}}]}"#;
        assert_eq!(parse(other, Path::new("/"), &[]), Some(vec![]));
        assert_eq!(parse(r#"{"Issues":null}"#, Path::new("/"), &[]), Some(vec![]));
    }

    #[test]
    fn no_report_is_none() {
        assert_eq!(parse("level=error msg=boom\n", Path::new("/"), &[]), None);
    }
}
