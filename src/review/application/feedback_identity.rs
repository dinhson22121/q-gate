use crate::review::domain::finding::{Finding, FindingSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingIdentity {
    pub file: String,
    pub category: String,
    pub source: String,
    pub evidence: String,
}

pub fn finding_identity(finding: &Finding) -> FindingIdentity {
    let source = match &finding.source {
        FindingSource::Llm => "llm".to_string(),
        FindingSource::StaticTool(name) => format!("tool:{name}"),
    };
    FindingIdentity {
        file: finding.file.clone(),
        category: finding.category.as_str().to_string(),
        source,
        evidence: finding.evidence.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::finding::Severity;
    use crate::review::domain::finding::fixtures::finding;

    #[test]
    fn llm_finding_identity() {
        let identity = finding_identity(&finding("a.go", 3, Severity::Major, "ev"));
        assert_eq!(
            identity,
            FindingIdentity {
                file: "a.go".into(),
                category: "correctness".into(),
                source: "llm".into(),
                evidence: "ev".into()
            }
        );
    }

    #[test]
    fn tool_finding_identity_names_the_tool() {
        let tool = Finding {
            source: FindingSource::StaticTool("gitleaks".into()),
            ..finding("a.go", 3, Severity::Major, "ev")
        };
        assert_eq!(finding_identity(&tool).source, "tool:gitleaks");
    }
}
