use super::impacted::ImpactedSet;
use super::test_run::TestOutcome;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub head_output_tail: String,
    pub base_outcome: TestOutcome,
    pub reproduce_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regression {
    pub package: String,
    pub test: String,
    pub evidence: Evidence,
    pub prevention_rule: String,
}

impl Regression {
    pub fn new(package: &str, test: &str, head_output_tail: String) -> Self {
        let subject = if test.is_empty() { format!("the {package} package tests") } else { test.to_string() };
        Self {
            package: package.to_string(),
            test: test.to_string(),
            evidence: Evidence {
                head_output_tail,
                base_outcome: TestOutcome::Pass,
                reproduce_command: reproduce_command(package, test),
            },
            prevention_rule: format!(
                "Changing {package} broke {subject}, which passed before; keep the contract that test guards \
                 (or update the test deliberately) and run `go test {package}` before pushing."
            ),
        }
    }
}

pub fn reproduce_command(package: &str, test: &str) -> String {
    if test.is_empty() {
        return format!("go test -count=1 {package}");
    }
    let pattern = test.split('/').map(|part| format!("^{}$", regex_escape(part))).collect::<Vec<_>>().join("/");
    format!("go test -count=1 -run '{pattern}' {package}")
}

fn regex_escape(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, c| {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegressionReport {
    pub regressions: Vec<Regression>,
    pub notes: Vec<String>,
    pub impacted: ImpactedSet,
    pub packages_tested: Vec<String>,
    pub packages_skipped: Vec<String>,
    pub skipped_reason: Option<String>,
}

impl RegressionReport {
    pub fn skipped(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            notes: vec![format!("regression check skipped: {reason}")],
            skipped_reason: Some(reason),
            ..Self::default()
        }
    }

    pub fn blocks(&self) -> bool {
        !self.regressions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reproduce_command_escapes_and_splits_subtests() {
        assert_eq!(reproduce_command("m/a", ""), "go test -count=1 m/a");
        assert_eq!(reproduce_command("m/a", "TestX"), "go test -count=1 -run '^TestX$' m/a");
        assert_eq!(reproduce_command("m/a", "TestX/a.b(1)"), "go test -count=1 -run '^TestX$/^a\\.b\\(1\\)$' m/a");
    }

    #[test]
    fn regression_carries_evidence_and_a_prevention_rule() {
        let r = Regression::new("m/a", "TestX", "boom".into());
        assert_eq!(r.evidence.base_outcome, TestOutcome::Pass);
        assert!(r.prevention_rule.contains("Changing m/a broke TestX"));
        assert!(Regression::new("m/a", "", String::new()).prevention_rule.contains("the m/a package tests"));
    }

    #[test]
    fn only_regressions_block() {
        let mut report = RegressionReport::skipped("no go");
        assert!(!report.blocks());
        assert_eq!(report.skipped_reason.as_deref(), Some("no go"));
        report.regressions.push(Regression::new("p", "T", String::new()));
        assert!(report.blocks());
    }
}
