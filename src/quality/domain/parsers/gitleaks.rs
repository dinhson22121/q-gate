//! gitleaks: `gitleaks dir . --redact --report-format json --report-path -` prints an array of leaks.

use std::path::Path;

use serde_json::Value;

use crate::quality::domain::finding::{QualityCategory, QualityFinding, QualitySeverity, QualityTool};
use crate::quality::domain::{json_documents, relative_to, secret_rule};

pub fn parse(output: &str, root: &Path) -> Option<Vec<QualityFinding>> {
    let report = json_documents(output).into_iter().find_map(|doc| match doc {
        Value::Array(items) => Some(items),
        _ => None,
    })?;
    Some(report.iter().filter_map(|leak| finding(leak, root)).collect())
}

fn finding(leak: &Value, root: &Path) -> Option<QualityFinding> {
    let rule_id = leak["RuleID"].as_str()?;
    let file = relative_to(leak["File"].as_str()?, root);
    let line = u32::try_from(leak["StartLine"].as_u64()?).ok()?;
    let description = leak["Description"].as_str().unwrap_or(rule_id).trim().to_string();
    Some(QualityFinding {
        tool: QualityTool::Gitleaks,
        category: QualityCategory::Secret,
        severity: QualitySeverity::Critical,
        file,
        line,
        identifier: rule_id.to_string(),
        aliases: vec![],
        title: format!("Possible leaked secret ({rule_id})"),
        // Never the matched value: gitleaks runs with --redact and this is the rule's description only.
        evidence: description,
        is_new: false,
        approved_rule: false,
        dependency: None,
        prevention_rule: secret_rule(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = include_str!("../../fixtures/gitleaks.json");

    #[test]
    fn parses_a_real_report_with_noise() {
        let output = format!("10:40AM INF scan completed\n{REPORT}");
        let findings = parse(&output, Path::new("/snap")).unwrap();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!((f.file.as_str(), f.line, f.identifier.as_str()), ("internal/domain/user.go", 8, "github-pat"));
        assert_eq!(f.severity, QualitySeverity::Critical);
        assert!(f.evidence.contains("GitHub Personal Access Token"));
        assert!(!f.is_new);
        assert!(f.prevention_rule.text.starts_with("Never commit secrets"));
    }

    #[test]
    fn absolute_paths_are_made_relative_and_the_secret_is_never_echoed() {
        let output = r#"[{"RuleID":"x","Description":"d","StartLine":3,"File":"/snap/a/b.go","Match":"SUPERSECRET","Secret":"SUPERSECRET"}]"#;
        let f = &parse(output, Path::new("/snap")).unwrap()[0];
        assert_eq!(f.file, "a/b.go");
        assert!(!format!("{f:?}").contains("SUPERSECRET"));
    }

    #[test]
    fn clean_report_is_empty_and_missing_report_is_none() {
        assert_eq!(parse("[]\n", Path::new("/")), Some(vec![]));
        assert_eq!(parse("fatal: boom\n", Path::new("/")), None);
        assert_eq!(parse(r#"[{"RuleID":"x"}]"#, Path::new("/")), Some(vec![]), "entries without a file are dropped");
    }
}
