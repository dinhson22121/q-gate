use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::json::leaf_paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Endpoint,
    McpCommand,
    RepoChecks,
    Webhook,
    GateLoosening,
}

impl Sensitivity {
    pub fn warning(self) -> &'static str {
        match self {
            Self::Endpoint => "changes where code, tickets or credentials are sent",
            Self::McpCommand => "changes the program qgate runs with your Jira credentials",
            Self::RepoChecks => "changes whether qgate runs commands from repository files",
            Self::Webhook => "changes where review results are posted",
            Self::GateLoosening => "loosens the gate: fewer problems will block a commit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldAccess {
    Settable(Option<Sensitivity>),
    AdminOnly,
    Credential,
    NotAllowed,
}

const SETTABLE_FIELDS: &[(&str, Option<Sensitivity>)] = &[
    ("llm.audit_findings", None),
    ("agents.enabled", None),
    ("agents.model", None),
    ("agents.effort", None),
    ("agents.max_parallel", None),
    ("agents.ticket_analyst.enabled", None),
    ("agents.ticket_analyst.include_parent_story", None),
    ("agents.ticket_analyst.use_description", None),
    ("agents.ticket_analyst.cache_ttl_minutes", None),
    ("agents.ticket_analyst.qa_link_type", None),
    ("agents.tests.enabled", None),
    ("agents.tests.languages", None),
    ("agents.tests.max_scenarios", None),
    ("agents.tests.max_repair_rounds", None),
    ("agents.tests.timeout_secs", None),
    ("agents.tests.on_fail", None),
    ("agents.architecture.enabled", None),
    ("agents.architecture.rulebook_refresh_days", None),
    ("agents.architecture.use_inferred_rules", None),
    ("agents.architecture.on_violation", None),
    ("figma.enabled", None),
    ("figma.provider", None),
    ("figma.checks", None),
    ("figma.block_on", None),
    ("jira.enabled", None),
    ("jira.ac_field", None),
    ("jira.key_pattern_projects", None),
    ("notify.webhook_url", Some(Sensitivity::Webhook)),
    ("watch.run_repo_checks", Some(Sensitivity::RepoChecks)),
];

const ADMIN_ONLY_FIELDS: &[&str] =
    &["llm.claude_bin", "llm.timeout_secs", "jira.base_url", "figma.api_base_url", "figma.mcp_url"];

pub const RUN_REPO_CHECKS_FIELD: &str = "watch.run_repo_checks";
pub const ADMIN_ONLY_REASON: &str =
    "admin-only: set via environment (qgate.env) and restart qgate; it cannot be changed over HTTP";

const SETTABLE_SECTIONS: &[&str] = &["output."];

const CREDENTIAL_MARKERS: &[&str] =
    &["secret", "token", "password", "api_key", "apikey", "credential", "env_passthrough", "private_key"];

pub const WEBHOOK_FIELD: &str = "notify.webhook_url";

pub fn classify(path: &str) -> FieldAccess {
    let lower = path.to_ascii_lowercase();
    if CREDENTIAL_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return FieldAccess::Credential;
    }
    if ADMIN_ONLY_FIELDS.contains(&path) {
        return FieldAccess::AdminOnly;
    }
    if let Some((_, sensitivity)) = SETTABLE_FIELDS.iter().find(|(field, _)| *field == path) {
        return FieldAccess::Settable(*sensitivity);
    }
    if SETTABLE_SECTIONS.iter().any(|section| path.starts_with(section)) {
        return FieldAccess::Settable(None);
    }
    FieldAccess::NotAllowed
}

pub fn settable_paths(settings: &Value) -> Vec<String> {
    leaf_paths(settings).into_iter().filter(|p| matches!(classify(p), FieldAccess::Settable(_))).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn classifies_paths_by_allow_list() {
        assert_eq!(classify("agents.tests.on_fail"), FieldAccess::Settable(None));
        assert_eq!(classify("llm.claude_bin"), FieldAccess::AdminOnly);
        assert_eq!(classify("jira.base_url"), FieldAccess::AdminOnly);
        assert_eq!(classify("figma.mcp_url"), FieldAccess::AdminOnly);
        assert_eq!(classify("watch.run_repo_checks"), FieldAccess::Settable(Some(Sensitivity::RepoChecks)));
        assert_eq!(classify("output.formats"), FieldAccess::Settable(None));
        assert_eq!(classify("secrets.jira_api_token"), FieldAccess::Credential);
        assert_eq!(classify("figma.token"), FieldAccess::Credential);
        assert_eq!(classify("dashboard.allowed_hosts"), FieldAccess::NotAllowed);
        assert_eq!(classify("jira.provider"), FieldAccess::NotAllowed);
    }

    #[test]
    fn lists_settable_leaves_and_warns_per_sensitivity() {
        let settings = json!({"agents": {"enabled": true}, "dashboard": {"bind": "x"}, "output": {"dir": "/d"}});
        assert_eq!(settable_paths(&settings), vec!["agents.enabled", "output.dir"]);
        for s in [
            Sensitivity::Endpoint,
            Sensitivity::McpCommand,
            Sensitivity::RepoChecks,
            Sensitivity::Webhook,
            Sensitivity::GateLoosening,
        ] {
            assert!(!s.warning().is_empty());
        }
    }
}
