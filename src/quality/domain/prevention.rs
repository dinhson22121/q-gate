use super::boundary::ImportBoundary;
use super::finding::{DependencyRef, QualityRule};

const SECRET_RULE: &str = "Never commit secrets (API keys, tokens, passwords, private keys); read them from environment variables or a secret manager and keep sample values obviously fake";

pub fn secret_rule() -> QualityRule {
    QualityRule { text: SECRET_RULE.to_string(), scope: None }
}

pub fn dependency_rule(dependency: &DependencyRef) -> QualityRule {
    let fix = match &dependency.fixed_version {
        Some(fixed) => format!("use {fixed} or newer"),
        None => "upgrade to a release without known vulnerabilities".to_string(),
    };
    let scope = dependency.manifest.as_deref().map(|path| format!("**/{}", path.rsplit('/').next().unwrap_or(path)));
    QualityRule {
        text: format!(
            "Do not depend on {} {}, which has known vulnerabilities: {fix}",
            dependency.name, dependency.version
        ),
        scope,
    }
}

pub fn architecture_rule(boundary: &ImportBoundary) -> QualityRule {
    let scope = match boundary.scope_globs.as_slice() {
        [only] => Some(format!("**/{}", only.trim_start_matches("**/").trim_start_matches('/'))),
        _ => None,
    };
    QualityRule { text: boundary.rule_text(), scope }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary(globs: &[&str]) -> ImportBoundary {
        ImportBoundary {
            id: "R1".into(),
            title: "t".into(),
            scope_globs: globs.iter().map(|g| g.to_string()).collect(),
            deny: vec!["x/infra".into()],
            approved: true,
        }
    }

    #[test]
    fn secret_rule_applies_everywhere() {
        let rule = secret_rule();
        assert_eq!(rule.scope, None);
        assert!(rule.text.starts_with("Never commit secrets"));
    }

    #[test]
    fn dependency_rule_names_the_fix_and_scopes_to_the_manifest() {
        let mut dep = DependencyRef {
            name: "golang.org/x/text".into(),
            version: "v0.3.7".into(),
            manifest: Some("svc/go.mod".into()),
            fixed_version: Some("v0.3.8".into()),
        };
        let rule = dependency_rule(&dep);
        assert_eq!(
            rule.text,
            "Do not depend on golang.org/x/text v0.3.7, which has known vulnerabilities: use v0.3.8 or newer"
        );
        assert_eq!(rule.scope.as_deref(), Some("**/go.mod"));
        dep.fixed_version = None;
        dep.manifest = None;
        let rule = dependency_rule(&dep);
        assert!(rule.text.ends_with("upgrade to a release without known vulnerabilities"));
        assert_eq!(rule.scope, None);
    }

    #[test]
    fn architecture_rule_scopes_to_a_single_glob() {
        let rule = architecture_rule(&boundary(&["internal/domain/**"]));
        assert_eq!(rule.text, "Code in internal/domain/** must not import x/infra");
        assert_eq!(rule.scope.as_deref(), Some("**/internal/domain/**"));
        assert_eq!(architecture_rule(&boundary(&["**/domain/**"])).scope.as_deref(), Some("**/domain/**"));
        assert_eq!(architecture_rule(&boundary(&["a/**", "b/**"])).scope, None);
    }
}
