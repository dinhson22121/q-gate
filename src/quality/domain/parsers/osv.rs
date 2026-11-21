//! osv-scanner: `osv-scanner scan source --format json -r .` prints `{"results":[{"source":..,"packages":[..]}]}`.
//! One finding per vulnerability group (a group is the same advisory under its CVE/GHSA/GO ids).

use std::path::Path;

use serde_json::Value;

use crate::quality::domain::finding::{DependencyRef, QualityCategory, QualityFinding, QualitySeverity, QualityTool};
use crate::quality::domain::{dependency_rule, json_documents, relative_to};

pub fn parse(output: &str, root: &Path) -> Option<Vec<QualityFinding>> {
    let report = json_documents(output).into_iter().find(|doc| doc.get("results").is_some())?;
    let results = report["results"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut findings = Vec::new();
    for result in results {
        let manifest = result["source"]["path"].as_str().map(|p| relative_to(p, root));
        for package in result["packages"].as_array().map(Vec::as_slice).unwrap_or_default() {
            findings.extend(package_findings(package, manifest.as_deref()));
        }
    }
    Some(findings)
}

fn package_findings(package: &Value, manifest: Option<&str>) -> Vec<QualityFinding> {
    let (Some(name), Some(version)) = (package["package"]["name"].as_str(), package["package"]["version"].as_str())
    else {
        return Vec::new();
    };
    let vulnerabilities = package["vulnerabilities"].as_array().map(Vec::as_slice).unwrap_or_default();
    let groups = package["groups"].as_array().map(Vec::as_slice).unwrap_or_default();
    groups
        .iter()
        .filter_map(|group| {
            let ids = strings(&group["ids"]);
            let aliases = strings(&group["aliases"]);
            let primary = aliases.iter().chain(&ids).find(|id| id.starts_with("CVE-")).or_else(|| ids.first())?.clone();
            let advisory =
                vulnerabilities.iter().find(|v| v["id"].as_str().is_some_and(|id| ids.iter().any(|i| i == id)));
            let dependency = DependencyRef {
                name: name.to_string(),
                version: version.to_string(),
                manifest: manifest.map(String::from),
                fixed_version: None,
            };
            let summary = advisory.and_then(|v| v["summary"].as_str()).unwrap_or("known vulnerability");
            Some(QualityFinding {
                tool: QualityTool::OsvScanner,
                category: QualityCategory::DependencyVuln,
                severity: severity(group, advisory),
                file: manifest.unwrap_or_default().to_string(),
                line: 0,
                identifier: primary.clone(),
                aliases: ids.iter().chain(&aliases).filter(|id| **id != primary).cloned().collect(),
                title: format!("{primary} in {name} {version}: {summary}"),
                evidence: format!("{name} {version}"),
                is_new: false,
                approved_rule: false,
                prevention_rule: dependency_rule(&dependency),
                dependency: Some(dependency),
            })
        })
        .collect()
}

/// CVSS score of the group, else the advisory's own label, else medium (unknown severity warns, never blocks).
fn severity(group: &Value, advisory: Option<&Value>) -> QualitySeverity {
    let score = group["max_severity"].as_str().and_then(|s| s.trim().parse::<f64>().ok());
    if let Some(score) = score {
        return QualitySeverity::from_cvss(score);
    }
    advisory
        .and_then(|v| v["database_specific"]["severity"].as_str())
        .and_then(QualitySeverity::from_label)
        .unwrap_or(QualitySeverity::Medium)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| items.iter().filter_map(|i| i.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = include_str!("../../fixtures/osv.json");

    #[test]
    fn parses_a_real_report() {
        let output = format!("Scanning dir .\n{REPORT}\nEnd status: 1 Extract calls\n");
        let findings = parse(&output, Path::new("/snap")).unwrap();
        assert_eq!(findings.len(), 3);
        let f = &findings[0];
        assert_eq!(f.identifier, "CVE-2022-32149");
        assert_eq!(f.severity, QualitySeverity::High);
        assert_eq!(f.file, "go.mod");
        assert!(
            f.aliases.contains(&"GO-2022-1059".to_string()) && f.aliases.contains(&"GHSA-69ch-w2m2-3vjp".to_string())
        );
        assert!(!f.aliases.contains(&"CVE-2022-32149".to_string()));
        assert_eq!(
            f.title,
            "CVE-2022-32149 in golang.org/x/text 0.3.7: Denial of service via crafted Accept-Language header in golang.org/x/text/language"
        );
        let dep = f.dependency.as_ref().unwrap();
        assert_eq!(
            (dep.name.as_str(), dep.version.as_str(), dep.manifest.as_deref()),
            ("golang.org/x/text", "0.3.7", Some("go.mod"))
        );
        assert_eq!(f.prevention_rule.scope.as_deref(), Some("**/go.mod"));
    }

    #[test]
    fn unscored_groups_default_to_medium_and_labels_are_used() {
        let findings = parse(REPORT, Path::new("/snap")).unwrap();
        assert_eq!(findings[1].severity, QualitySeverity::Medium);
        let labelled = r#"{"results":[{"source":{"path":"/snap/pom.xml"},"packages":[{"package":{"name":"g:a","version":"1.0.0"},
            "groups":[{"ids":["GHSA-x"],"aliases":["GHSA-x"],"max_severity":""}],
            "vulnerabilities":[{"id":"GHSA-x","summary":"s","database_specific":{"severity":"CRITICAL"}}]}]}]}"#;
        let f = &parse(labelled, Path::new("/snap")).unwrap()[0];
        assert_eq!(
            (f.severity, f.identifier.as_str(), f.file.as_str()),
            (QualitySeverity::Critical, "GHSA-x", "pom.xml")
        );
    }

    #[test]
    fn empty_and_missing_reports() {
        assert_eq!(parse(r#"{"results":[]}"#, Path::new("/")), Some(vec![]));
        assert_eq!(parse("No package sources found\n", Path::new("/")), None);
        let broken = r#"{"results":[{"packages":[{"package":{"name":"x"},"groups":[{"ids":["A"]}]}]}]}"#;
        assert_eq!(parse(broken, Path::new("/")), Some(vec![]), "package without a version is skipped");
    }
}
