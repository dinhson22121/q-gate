pub const DIRECTIVE: &str = "qgate:go-import-deny";
const SCOPE_KEY: &str = "scope=";
const DENY_KEY: &str = "deny=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBoundary {
    pub id: String,
    pub title: String,
    pub scope_globs: Vec<String>,
    pub deny: Vec<String>,
    pub approved: bool,
}

impl ImportBoundary {
    pub fn rule_text(&self) -> String {
        format!("Code in {} must not import {}", self.scope_globs.join(", "), self.deny.join(", "))
    }
}

pub fn parse_boundary_directives(rule_id: &str, title: &str, approved: bool, texts: &[&str]) -> Vec<ImportBoundary> {
    let base = sanitize_id(rule_id);
    let parsed: Vec<(Vec<String>, Vec<String>)> =
        texts.iter().flat_map(|text| text.lines()).filter_map(parse_line).collect();
    let many = parsed.len() > 1;
    parsed
        .into_iter()
        .enumerate()
        .map(|(index, (scope_globs, deny))| ImportBoundary {
            id: if many && index > 0 { format!("{base}-{}", index + 1) } else { base.clone() },
            title: title.to_string(),
            scope_globs,
            deny,
            approved,
        })
        .collect()
}

fn parse_line(line: &str) -> Option<(Vec<String>, Vec<String>)> {
    let (_, args) = line.split_once(DIRECTIVE)?;
    let mut scope = None;
    let mut deny = None;
    for token in args.split_whitespace() {
        if let Some(value) = token.strip_prefix(SCOPE_KEY) {
            scope = safe_list(value);
        } else if let Some(value) = token.strip_prefix(DENY_KEY) {
            deny = safe_list(value);
        }
    }
    Some((scope?, deny?))
}

fn safe_list(value: &str) -> Option<Vec<String>> {
    let items: Vec<String> =
        value.split(',').map(|item| item.trim_matches(['`', '"', '\'', '.'])).map(String::from).collect();
    let valid = !items.is_empty()
        && items.iter().all(|item| {
            !item.is_empty() && item.chars().all(|c| c.is_ascii_alphanumeric() || "*?/._-{}$@+~".contains(c))
        });
    valid.then_some(items)
}

fn sanitize_id(id: &str) -> String {
    let cleaned: String =
        id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect();
    if cleaned.is_empty() { "rule".to_string() } else { cleaned }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scope_and_deny_lists() {
        let text = "Domain stays pure.\nqgate:go-import-deny scope=internal/domain/**,pkg/core/** deny={module}/internal/infra,database/sql\n";
        let boundaries = parse_boundary_directives("R3", "Pure domain", true, &["", text]);
        assert_eq!(
            boundaries,
            vec![ImportBoundary {
                id: "R3".into(),
                title: "Pure domain".into(),
                scope_globs: vec!["internal/domain/**".into(), "pkg/core/**".into()],
                deny: vec!["{module}/internal/infra".into(), "database/sql".into()],
                approved: true,
            }]
        );
        assert_eq!(
            boundaries[0].rule_text(),
            "Code in internal/domain/**, pkg/core/** must not import {module}/internal/infra, database/sql"
        );
    }

    #[test]
    fn several_directives_get_numbered_ids() {
        let text = "qgate:go-import-deny scope=a/** deny=x\nqgate:go-import-deny scope=b/** deny=y";
        let ids: Vec<_> = parse_boundary_directives("R 1", "t", false, &[text]).into_iter().map(|b| b.id).collect();
        assert_eq!(ids, ["R-1", "R-1-2"]);
    }

    #[test]
    fn malformed_or_unsafe_directives_are_ignored() {
        for text in [
            "qgate:go-import-deny scope=a/**",
            "qgate:go-import-deny deny=x",
            "qgate:go-import-deny scope=a/** deny=x\"; evil",
            "qgate:go-import-deny scope= deny=x",
            "no directive here",
        ] {
            assert!(parse_boundary_directives("R1", "t", true, &[text]).is_empty(), "{text}");
        }
        assert_eq!(parse_boundary_directives("", "t", true, &["qgate:go-import-deny scope=a deny=b"])[0].id, "rule");
    }
}
