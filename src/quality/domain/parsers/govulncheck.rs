use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::quality::domain::finding::{DependencyRef, QualityCategory, QualityFinding, QualitySeverity, QualityTool};
use crate::quality::domain::{dependency_rule, json_documents, relative_to};

const MANIFEST: &str = "go.mod";

struct Advisory {
    aliases: Vec<String>,
    summary: String,
}

pub fn parse(output: &str, root: &Path) -> Option<Vec<QualityFinding>> {
    let documents = json_documents(output);
    if !documents.iter().any(|d| d.get("config").is_some() || d.get("finding").is_some() || d.get("osv").is_some()) {
        return None;
    }
    let advisories: HashMap<String, Advisory> = documents.iter().filter_map(|d| advisory(&d["osv"])).collect();
    let mut seen = std::collections::HashSet::new();
    let findings = documents
        .iter()
        .filter_map(|d| finding(&d["finding"], root, &advisories))
        .filter(|f| seen.insert(f.identifier.clone() + &f.file + &f.line.to_string()))
        .collect();
    Some(findings)
}

fn advisory(osv: &Value) -> Option<(String, Advisory)> {
    let id = osv["id"].as_str()?.to_string();
    let aliases = osv["aliases"].as_array()?.iter().filter_map(|a| a.as_str().map(String::from)).collect();
    Some((id, Advisory { aliases, summary: osv["summary"].as_str().unwrap_or_default().to_string() }))
}

fn finding(message: &Value, root: &Path, advisories: &HashMap<String, Advisory>) -> Option<QualityFinding> {
    let osv_id = message["osv"].as_str()?;
    let trace = message["trace"].as_array()?;
    let vulnerable = trace.first().filter(|frame| frame["function"].is_string())?;
    let module = vulnerable["module"].as_str()?;
    let version = vulnerable["version"].as_str().unwrap_or_default();
    let call_site = trace.iter().rev().find_map(|frame| {
        let position = frame.get("position")?;
        Some((relative_to(position["filename"].as_str()?, root), u32::try_from(position["line"].as_u64()?).ok()?))
    });
    let advisory = advisories.get(osv_id);
    let primary = advisory
        .and_then(|a| a.aliases.iter().find(|alias| alias.starts_with("CVE-")))
        .cloned()
        .unwrap_or_else(|| osv_id.to_string());
    let dependency = DependencyRef {
        name: module.to_string(),
        version: version.to_string(),
        manifest: None,
        fixed_version: message["fixed_version"].as_str().map(String::from),
    };
    let (file, line) = call_site.unwrap_or_else(|| (MANIFEST.to_string(), 0));
    let function = vulnerable["function"].as_str().unwrap_or_default();
    let summary = advisory.map(|a| a.summary.as_str()).filter(|s| !s.is_empty()).unwrap_or("known vulnerability");
    Some(QualityFinding {
        tool: QualityTool::Govulncheck,
        category: QualityCategory::DependencyVuln,
        severity: QualitySeverity::High,
        file,
        line,
        aliases: std::iter::once(osv_id.to_string())
            .chain(advisory.into_iter().flat_map(|a| a.aliases.iter().cloned()))
            .filter(|id| *id != primary)
            .collect(),
        title: format!("{primary} in {module} {version} is reachable via {function}: {summary}"),
        evidence: format!("{module} {version} ({function})"),
        identifier: primary,
        is_new: false,
        approved_rule: false,
        prevention_rule: dependency_rule(&dependency),
        dependency: Some(dependency),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = include_str!("../../fixtures/govulncheck.json");

    #[test]
    fn reports_only_reachable_findings_from_a_real_stream() {
        let findings = parse(STREAM, Path::new("/snap")).unwrap();
        assert_eq!(findings.len(), 1, "module/package level findings are left to osv-scanner");
        let f = &findings[0];
        assert_eq!(f.identifier, "CVE-2022-32149");
        assert_eq!(f.severity, QualitySeverity::High);
        assert_eq!((f.file.as_str(), f.line), ("main.go", 10));
        assert!(f.aliases.contains(&"GO-2022-1059".to_string()));
        let dep = f.dependency.as_ref().unwrap();
        assert_eq!((dep.name.as_str(), dep.fixed_version.as_deref()), ("golang.org/x/text", Some("v0.3.8")));
        assert!(f.title.contains("reachable via ParseAcceptLanguage"));
        assert_eq!(
            f.prevention_rule.text,
            "Do not depend on golang.org/x/text v0.3.7, which has known vulnerabilities: use v0.3.8 or newer"
        );
    }

    #[test]
    fn missing_call_site_points_at_go_mod() {
        let stream = r#"{"finding":{"osv":"GO-1","trace":[{"module":"m","version":"v1","function":"F"}]}}"#;
        let f = &parse(stream, Path::new("/snap")).unwrap()[0];
        assert_eq!((f.file.as_str(), f.line, f.identifier.as_str()), ("go.mod", 0, "GO-1"));
    }

    #[test]
    fn no_findings_and_no_report() {
        assert_eq!(parse("{\"config\":{}}\n", Path::new("/")), Some(vec![]));
        assert_eq!(parse("govulncheck: loading packages: boom\n", Path::new("/")), None);
    }
}
