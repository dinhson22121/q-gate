//! A deterministic finding and how it is graded for the gate.

use serde::Serialize;

/// Languages the quality checks know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Go,
    Java,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::Java => "java",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityTool {
    Gitleaks,
    OsvScanner,
    Govulncheck,
    Depguard,
}

impl QualityTool {
    pub const ALL: [Self; 4] = [Self::Gitleaks, Self::OsvScanner, Self::Govulncheck, Self::Depguard];

    /// Name shown to people (and recorded as the finding's source).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gitleaks => "gitleaks",
            Self::OsvScanner => "osv-scanner",
            Self::Govulncheck => "govulncheck",
            Self::Depguard => "depguard",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityCategory {
    DependencyVuln,
    Secret,
    Architecture,
}

impl QualityCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DependencyVuln => "dependency_vuln",
            Self::Secret => "secret",
            Self::Architecture => "architecture",
        }
    }
}

/// Ordered from least to most severe. CVSS buckets for vulnerabilities; secrets are always critical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualitySeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl QualitySeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// Bucket of a CVSS base score (<4 low, <7 medium, <9 high, else critical).
    pub fn from_cvss(score: f64) -> Self {
        if score >= 9.0 {
            Self::Critical
        } else if score >= 7.0 {
            Self::High
        } else if score >= 4.0 {
            Self::Medium
        } else {
            Self::Low
        }
    }

    /// Advisory-database label (GitHub: LOW/MODERATE/HIGH/CRITICAL); `None` when unrecognised.
    pub fn from_label(label: &str) -> Option<Self> {
        match label.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" | "moderate" => Some(Self::Medium),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// What the gate should do with a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Block,
    Warn,
}

/// A general rule for the developer's coding agent that would have prevented the finding (same shape as the
/// review's prevention rules: one imperative line and an optional file glob).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualityRule {
    pub text: String,
    pub scope: Option<String>,
}

/// The package a vulnerability is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyRef {
    /// Go module path or Maven `group:artifact`.
    pub name: String,
    pub version: String,
    /// Manifest or lockfile the dependency comes from (repo-relative); `None` when the tool does not say.
    pub manifest: Option<String>,
    pub fixed_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualityFinding {
    pub tool: QualityTool,
    pub category: QualityCategory,
    pub severity: QualitySeverity,
    /// Repo-relative path.
    pub file: String,
    /// 1-based; 0 when the finding has no line (a dependency in a manifest).
    pub line: u32,
    /// CVE/GHSA/GO id, gitleaks rule id, or the architecture rule id.
    pub identifier: String,
    /// Other ids of the same vulnerability (used to merge the reports of several tools).
    pub aliases: Vec<String>,
    pub title: String,
    /// What was found, never a secret's value.
    pub evidence: String,
    /// Introduced by the diff; everything else is shown for reference and never blocks.
    pub is_new: bool,
    /// Architecture findings only: the violated rule is an approved Rulebook/team rule.
    pub approved_rule: bool,
    pub dependency: Option<DependencyRef>,
    pub prevention_rule: QualityRule,
}

impl QualityFinding {
    /// Blocks only when introduced by the diff: any secret, a vulnerability of at least high severity, a violation
    /// of an approved architecture rule. Everything else warns.
    pub fn gate_verdict(&self) -> Verdict {
        if !self.is_new {
            return Verdict::Warn;
        }
        let blocks = match self.category {
            QualityCategory::Secret => true,
            QualityCategory::DependencyVuln => self.severity >= QualitySeverity::High,
            QualityCategory::Architecture => self.approved_rule,
        };
        if blocks { Verdict::Block } else { Verdict::Warn }
    }

    pub fn is_blocking(&self) -> bool {
        self.gate_verdict() == Verdict::Block
    }

    /// Every id this finding is known by.
    pub fn all_ids(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.identifier.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn finding(category: QualityCategory, severity: QualitySeverity, is_new: bool) -> QualityFinding {
        QualityFinding {
            tool: QualityTool::OsvScanner,
            category,
            severity,
            file: "go.mod".into(),
            line: 0,
            identifier: "CVE-2022-1".into(),
            aliases: vec![],
            title: "t".into(),
            evidence: "e".into(),
            is_new,
            approved_rule: false,
            dependency: None,
            prevention_rule: QualityRule { text: "r".into(), scope: None },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::finding;
    use super::*;

    #[test]
    fn cvss_buckets() {
        assert_eq!(QualitySeverity::from_cvss(9.8), QualitySeverity::Critical);
        assert_eq!(QualitySeverity::from_cvss(7.5), QualitySeverity::High);
        assert_eq!(QualitySeverity::from_cvss(4.0), QualitySeverity::Medium);
        assert_eq!(QualitySeverity::from_cvss(3.9), QualitySeverity::Low);
    }

    #[test]
    fn labels_parse_case_insensitively() {
        assert_eq!(QualitySeverity::from_label(" MODERATE "), Some(QualitySeverity::Medium));
        assert_eq!(QualitySeverity::from_label("HIGH"), Some(QualitySeverity::High));
        assert_eq!(QualitySeverity::from_label("unknown"), None);
    }

    #[test]
    fn names_are_stable() {
        assert_eq!(QualityTool::ALL.map(QualityTool::as_str), ["gitleaks", "osv-scanner", "govulncheck", "depguard"]);
        assert_eq!(QualityCategory::DependencyVuln.as_str(), "dependency_vuln");
        assert_eq!(QualityCategory::Secret.as_str(), "secret");
        assert_eq!(QualityCategory::Architecture.as_str(), "architecture");
        assert_eq!(QualitySeverity::Critical.as_str(), "critical");
        assert_eq!(Language::Go.as_str(), "go");
        assert_eq!(Language::Java.as_str(), "java");
    }

    #[test]
    fn new_secret_always_blocks() {
        assert_eq!(finding(QualityCategory::Secret, QualitySeverity::Low, true).gate_verdict(), Verdict::Block);
    }

    #[test]
    fn pre_existing_findings_never_block() {
        for category in [QualityCategory::Secret, QualityCategory::DependencyVuln, QualityCategory::Architecture] {
            let mut f = finding(category, QualitySeverity::Critical, false);
            f.approved_rule = true;
            assert_eq!(f.gate_verdict(), Verdict::Warn, "{category:?}");
        }
    }

    #[test]
    fn new_vulnerability_blocks_from_high_up() {
        let verdict = |s| finding(QualityCategory::DependencyVuln, s, true).gate_verdict();
        assert_eq!(verdict(QualitySeverity::Critical), Verdict::Block);
        assert_eq!(verdict(QualitySeverity::High), Verdict::Block);
        assert_eq!(verdict(QualitySeverity::Medium), Verdict::Warn);
        assert_eq!(verdict(QualitySeverity::Low), Verdict::Warn);
    }

    #[test]
    fn architecture_blocks_only_for_approved_rules() {
        let mut f = finding(QualityCategory::Architecture, QualitySeverity::Medium, true);
        assert!(!f.is_blocking());
        f.approved_rule = true;
        assert!(f.is_blocking());
    }

    #[test]
    fn all_ids_lists_identifier_then_aliases() {
        let mut f = finding(QualityCategory::DependencyVuln, QualitySeverity::High, true);
        f.aliases = vec!["GO-1".into()];
        assert_eq!(f.all_ids().collect::<Vec<_>>(), ["CVE-2022-1", "GO-1"]);
    }
}
