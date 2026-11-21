//! Renders a proposal as a diff: server settings before/after, rulebook additions and edits, and the
//! `.qgate.toml` patch the developer commits themselves (qgate never writes to repositories).

use serde::Serialize;
use serde_json::Value;

use super::fields::WEBHOOK_FIELD;
use super::proposal::{CheckSuggestion, GateChange, ProposalContent, RuleAction};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Added,
    Removed,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewSection {
    pub title: String,
    pub lines: Vec<DiffLine>,
}

fn line(kind: LineKind, text: impl Into<String>) -> DiffLine {
    DiffLine { kind, text: text.into() }
}

fn display(value: &Value) -> String {
    value.to_string()
}

pub fn render_preview(content: &ProposalContent, repo: Option<&str>, webhook_configured: bool) -> Vec<PreviewSection> {
    let mut sections = Vec::new();
    if !content.settings.is_empty() {
        sections.push(settings_section(content, webhook_configured));
    }
    if let (Some(repo), false) = (repo, content.rules.is_empty()) {
        sections.push(rules_section(content, repo));
    }
    if let Some(patch) = repo_patch(&content.gate, &content.checks) {
        let mut lines = vec![line(
            LineKind::Note,
            "Suggested edits to .qgate.toml: qgate never writes to repositories; commit this yourself.",
        )];
        lines.extend(patch.lines().map(|text| match text.chars().next() {
            Some('+') => line(LineKind::Added, text),
            Some('-') => line(LineKind::Removed, text),
            _ => line(LineKind::Context, text),
        }));
        sections.push(PreviewSection { title: format!("Repository patch ({})", repo.unwrap_or("?")), lines });
    }
    sections
}

fn settings_section(content: &ProposalContent, webhook_configured: bool) -> PreviewSection {
    let mut lines = Vec::new();
    for change in &content.settings {
        let old = if change.path == WEBHOOK_FIELD {
            (if webhook_configured { "(configured, hidden)" } else { "(not set)" }).to_string()
        } else {
            display(&change.old)
        };
        lines.push(line(LineKind::Removed, format!("- {} = {old}", change.path)));
        lines.push(line(LineKind::Added, format!("+ {} = {}", change.path, display(&change.new))));
        for sensitivity in &change.sensitivity {
            lines.push(line(LineKind::Note, format!("  ⚠ {}", sensitivity.warning())));
        }
        if change.restart_required {
            lines.push(line(LineKind::Note, "  takes effect after `docker compose restart qgate`"));
        }
    }
    PreviewSection { title: "Server settings (new settings version)".into(), lines }
}

fn rules_section(content: &ProposalContent, repo: &str) -> PreviewSection {
    let mut lines = Vec::new();
    for rule in &content.rules {
        if let (RuleAction::Edit, Some((id, title))) = (rule.action, &rule.replaces) {
            lines.push(line(LineKind::Removed, format!("- {id} {title}")));
        }
        let scope = if rule.scope_globs.is_empty() { "all files".to_string() } else { rule.scope_globs.join(", ") };
        lines.push(line(LineKind::Added, format!("+ [team] {} (scope: {scope})", rule.title)));
        lines.push(line(LineKind::Context, format!("  {}", rule.description)));
    }
    lines
        .push(line(LineKind::Note, "New rules are judged by the model on future commits; history cannot replay them."));
    PreviewSection { title: format!("Rulebook ({repo})"), lines }
}

fn toml_value(value: &Value) -> String {
    toml::Value::try_from(value).map(|v| v.to_string()).unwrap_or_else(|_| value.to_string())
}

/// The `.qgate.toml` edits as diff text, or `None` when there is nothing to change in the repository.
pub fn repo_patch(gate: &[GateChange], checks: &[CheckSuggestion]) -> Option<String> {
    if gate.is_empty() && checks.is_empty() {
        return None;
    }
    let mut out = vec!["--- a/.qgate.toml".to_string(), "+++ b/.qgate.toml".to_string()];
    if !gate.is_empty() {
        out.push(" [gate]".into());
        for change in gate {
            out.push(format!("-{} = {}", change.field.as_str(), toml_value(&change.old)));
            out.push(format!("+{} = {}", change.field.as_str(), toml_value(&change.new)));
        }
    }
    for check in checks {
        out.push("+".into());
        out.push("+[[checks]]".into());
        out.push(format!("+name = {}", toml_value(&Value::from(check.name.as_str()))));
        let command: Vec<Value> = check.command.iter().map(|c| Value::from(c.as_str())).collect();
        out.push(format!("+command = {}", toml_value(&Value::Array(command))));
        out.push(format!("+required = {}", check.required));
        out.push(format!("+timeout_secs = {}", check.timeout_secs));
    }
    Some(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::policy::domain::fields::Sensitivity;
    use crate::policy::domain::gate::GateField;
    use crate::policy::domain::proposal::{RuleChange, SettingChange};

    fn content() -> ProposalContent {
        ProposalContent {
            settings: vec![
                SettingChange {
                    path: "agents.tests.on_fail".into(),
                    old: json!("block"),
                    new: json!("warn"),
                    reason: String::new(),
                    sensitivity: vec![Sensitivity::GateLoosening],
                    restart_required: false,
                },
                SettingChange {
                    path: WEBHOOK_FIELD.into(),
                    old: Value::Null,
                    new: json!("https://hooks.example/x"),
                    reason: String::new(),
                    sensitivity: vec![Sensitivity::Webhook],
                    restart_required: true,
                },
            ],
            rules: vec![
                RuleChange {
                    action: RuleAction::Add,
                    replaces: None,
                    title: "No SQL in handlers".into(),
                    description: "Use repositories.".into(),
                    scope_globs: vec![],
                    reason: String::new(),
                },
                RuleChange {
                    action: RuleAction::Edit,
                    replaces: Some(("R2".into(), "Old".into())),
                    title: "New".into(),
                    description: "d".into(),
                    scope_globs: vec!["src/**".into()],
                    reason: String::new(),
                },
            ],
            gate: vec![GateChange {
                field: GateField::FailOn,
                old: json!("critical"),
                new: json!("major"),
                reason: String::new(),
                loosening: false,
            }],
            checks: vec![CheckSuggestion {
                name: "lint \"strict\"".into(),
                command: vec!["cargo".into(), "clippy".into()],
                required: true,
                timeout_secs: 600,
                reason: String::new(),
            }],
            ..ProposalContent::default()
        }
    }

    fn texts(section: &PreviewSection) -> Vec<&str> {
        section.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn renders_settings_rules_and_repo_patch_as_diff() {
        let sections = render_preview(&content(), Some("/repos/app"), true);
        assert_eq!(sections.len(), 3);
        let settings = texts(&sections[0]);
        assert!(settings.contains(&"- agents.tests.on_fail = \"block\""));
        assert!(settings.contains(&"+ agents.tests.on_fail = \"warn\""));
        assert!(settings.contains(&"- notify.webhook_url = (configured, hidden)"));
        assert!(settings.iter().any(|t| t.contains("loosens the gate")));
        assert!(settings.iter().any(|t| t.contains("restart")));
        let rules = texts(&sections[1]);
        assert_eq!(sections[1].title, "Rulebook (/repos/app)");
        assert!(rules.contains(&"+ [team] No SQL in handlers (scope: all files)"));
        assert!(rules.contains(&"- R2 Old"));
        assert_eq!(sections[1].lines.last().unwrap().kind, LineKind::Note);
        let patch = &sections[2];
        assert!(patch.lines.iter().any(|l| l.kind == LineKind::Added && l.text == "+fail_on = \"major\""));
        assert!(patch.lines.iter().any(|l| l.kind == LineKind::Removed && l.text == "-fail_on = \"critical\""));
        assert!(
            texts(patch).contains(&"+name = 'lint \"strict\"'")
                || texts(patch).contains(&"+name = \"lint \\\"strict\\\"\"")
        );
        assert!(texts(patch).contains(&"+command = [\"cargo\", \"clippy\"]"));
    }

    #[test]
    fn empty_parts_render_nothing() {
        let mut only_settings = content();
        only_settings.rules.clear();
        only_settings.gate.clear();
        only_settings.checks.clear();
        let sections = render_preview(&only_settings, None, false);
        assert_eq!(sections.len(), 1);
        assert!(texts(&sections[0]).contains(&"- notify.webhook_url = (not set)"));
        assert_eq!(repo_patch(&[], &[]), None);
        assert!(render_preview(&content(), None, false).iter().all(|s| !s.title.starts_with("Rulebook")));
    }
}
