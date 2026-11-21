use crate::impact::application::Regression;
use crate::quality::{QualityCategory, QualityFinding, Verdict};
use crate::review::domain::finding::{Category, Confidence, Finding, FindingSource, PreventionRule, Severity};

const REGRESSION_TOOL: &str = "regression";
const FIRST_LINE: u32 = 1;

pub fn quality_finding_to_finding(found: &QualityFinding) -> Finding {
    let (severity, confidence) = match found.gate_verdict() {
        Verdict::Block => (Severity::Critical, Confidence::High),
        Verdict::Warn => (Severity::Minor, Confidence::Medium),
    };
    let category = match found.category {
        QualityCategory::DependencyVuln | QualityCategory::Secret => Category::Security,
        QualityCategory::Architecture => Category::Architecture,
    };
    let origin = if found.is_new { "introduced by this change" } else { "pre-existing, shown for reference" };
    let suggestion = found
        .dependency
        .as_ref()
        .and_then(|d| d.fixed_version.as_ref().map(|v| format!("Upgrade {} to {v} or later.", d.name)));
    Finding {
        file: found.file.clone(),
        line: found.line.max(FIRST_LINE),
        severity,
        category,
        confidence,
        title: found.title.clone(),
        explanation: format!("{} reported {} ({origin}).", found.tool.as_str(), found.identifier),
        evidence: found.evidence.clone(),
        suggestion,
        prevention_rule: PreventionRule::new(
            &found.prevention_rule.text,
            found.prevention_rule.scope.as_deref().unwrap_or_default(),
        ),
        source: FindingSource::StaticTool(found.tool.as_str().to_string()),
    }
}

pub fn regression_to_finding(regression: &Regression, location: Option<(String, u32)>) -> Finding {
    let (file, line) = location.unwrap_or_else(|| (regression.package.clone(), FIRST_LINE));
    let subject = if regression.test.is_empty() { "package tests".to_string() } else { regression.test.clone() };
    let evidence = &regression.evidence;
    Finding {
        file,
        line,
        severity: Severity::Critical,
        category: Category::Correctness,
        confidence: Confidence::High,
        title: format!("Regression: {subject} fails after this change"),
        explanation: format!(
            "{subject} in {} passed on the base commit and fails on this one (confirmed by a re-run).",
            regression.package
        ),
        evidence: format!(
            "{}\npassed on base; reproduce: {}",
            evidence.head_output_tail.trim_end(),
            evidence.reproduce_command
        ),
        suggestion: Some(format!(
            "Reproduce with `{}` and fix the change or update the test deliberately.",
            evidence.reproduce_command
        )),
        prevention_rule: PreventionRule::new(&regression.prevention_rule, ""),
        source: FindingSource::StaticTool(REGRESSION_TOOL.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::{DependencyRef, QualityRule, QualitySeverity, QualityTool};
    use crate::review::domain::gate::GatePolicy;

    fn quality(category: QualityCategory, severity: QualitySeverity, is_new: bool) -> QualityFinding {
        QualityFinding {
            tool: QualityTool::OsvScanner,
            category,
            severity,
            file: "go.mod".into(),
            line: 0,
            identifier: "CVE-2022-1".into(),
            aliases: vec![],
            title: "Vulnerable dependency".into(),
            evidence: "golang.org/x/text v0.3.0".into(),
            is_new,
            approved_rule: false,
            dependency: Some(DependencyRef {
                name: "golang.org/x/text".into(),
                version: "v0.3.0".into(),
                manifest: Some("go.mod".into()),
                fixed_version: Some("v0.3.8".into()),
            }),
            prevention_rule: QualityRule { text: "Keep dependencies patched".into(), scope: Some("**/go.mod".into()) },
        }
    }

    #[test]
    fn blocking_quality_finding_blocks_the_default_gate() {
        let found = quality(QualityCategory::DependencyVuln, QualitySeverity::High, true);
        let finding = quality_finding_to_finding(&found);
        assert_eq!(finding.category, Category::Security);
        assert_eq!((finding.severity, finding.confidence), (Severity::Critical, Confidence::High));
        assert!(GatePolicy::default().is_blocking(&finding));
        assert_eq!(finding.line, 1, "a manifest finding has no line of its own");
        assert_eq!(finding.source, FindingSource::StaticTool("osv-scanner".into()));
        assert_eq!(finding.suggestion.as_deref(), Some("Upgrade golang.org/x/text to v0.3.8 or later."));
        assert!(finding.explanation.contains("CVE-2022-1") && finding.explanation.contains("introduced"));
    }

    #[test]
    fn warning_quality_findings_never_block() {
        let reference = quality(QualityCategory::DependencyVuln, QualitySeverity::Critical, false);
        let low = quality(QualityCategory::DependencyVuln, QualitySeverity::Low, true);
        for found in [reference, low] {
            let finding = quality_finding_to_finding(&found);
            assert_eq!(finding.severity, Severity::Minor);
            assert!(!GatePolicy::default().is_blocking(&finding));
        }
    }

    #[test]
    fn prevention_rule_is_carried_with_its_scope() {
        let finding = quality_finding_to_finding(&quality(QualityCategory::Secret, QualitySeverity::Critical, true));
        let rule = finding.prevention_rule.unwrap();
        assert_eq!(rule.text, "Keep dependencies patched");
        assert_eq!(rule.scope.as_deref(), Some("**/go.mod"));
    }

    #[test]
    fn architecture_and_secret_categories_map_and_block_when_approved() {
        let mut arch = quality(QualityCategory::Architecture, QualitySeverity::Medium, true);
        arch.dependency = None;
        arch.line = 12;
        assert_eq!(quality_finding_to_finding(&arch).severity, Severity::Minor, "proposed rule only warns");
        arch.approved_rule = true;
        let finding = quality_finding_to_finding(&arch);
        assert_eq!((finding.category, finding.line, finding.suggestion.clone()), (Category::Architecture, 12, None));
        assert!(GatePolicy::default().is_blocking(&finding));
        let secret = quality_finding_to_finding(&quality(QualityCategory::Secret, QualitySeverity::Critical, true));
        assert_eq!(secret.category, Category::Security);
        assert!(GatePolicy::default().is_blocking(&secret));
    }

    #[test]
    fn regression_is_a_blocking_correctness_finding_with_reproduction() {
        let regression = Regression::new("shop/cart", "TestTotal", "--- FAIL: TestTotal\n  got 1 want 2\n".into());
        let finding = regression_to_finding(&regression, None);
        assert_eq!(finding.category, Category::Correctness);
        assert_eq!(finding.file, "shop/cart");
        assert!(GatePolicy::default().is_blocking(&finding));
        assert!(finding.evidence.starts_with("--- FAIL: TestTotal"));
        assert!(finding.evidence.ends_with("passed on base; reproduce: go test -count=1 -run '^TestTotal$' shop/cart"));
        assert!(finding.title.contains("TestTotal"));
        assert_eq!(
            finding.prevention_rule.unwrap().text,
            regression.prevention_rule.chars().take(300).collect::<String>()
        );
        assert_eq!(finding.source, FindingSource::StaticTool("regression".into()));
    }

    #[test]
    fn regression_points_at_the_failing_test_when_it_was_located() {
        let regression = Regression::new("shop/cart", "TestTotal", String::new());
        let finding = regression_to_finding(&regression, Some(("cart/cart_test.go".into(), 42)));
        assert_eq!((finding.file.as_str(), finding.line), ("cart/cart_test.go", 42));
        assert!(finding.explanation.contains("shop/cart"), "the package stays in the explanation");
    }

    #[test]
    fn package_level_regression_names_the_package_tests() {
        let finding = regression_to_finding(&Regression::new("shop/cart", "", String::new()), None);
        assert!(finding.title.contains("package tests"));
    }
}
