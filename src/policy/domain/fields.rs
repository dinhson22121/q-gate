//! Which server settings the Policy assistant may change, and which changes need an extra confirmation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::json::leaf_paths;

/// Why a change needs the user's separate, explicit confirmation before it is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Decides where code, tickets or credentials are sent.
    Endpoint,
    /// Chooses a program qgate runs with Jira credentials.
    McpCommand,
    /// Running repository-controlled `[[checks]]` commands.
    RepoChecks,
    /// Review results are posted to this URL.
    Webhook,
    /// Fewer problems would block a commit.
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

/// How the assistant may treat one settings path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldAccess {
    Settable(Option<Sensitivity>),
    /// Endpoints and commands: only an administrator sets them (environment / qgate.env), never over HTTP.
    AdminOnly,
    /// Credentials are never set through the assistant (use the Settings page).
    Credential,
    NotAllowed,
}

/// Exact paths the assistant may change, with the confirmation they need.
const SETTABLE_FIELDS: &[(&str, Option<Sensitivity>)] = &[
    ("agents.enabled", None),
    ("agents.model", None),
    ("agents.effort", None),
    ("agents.max_parallel", None),
    ("agents.budget_usd_per_review", None),
    ("agents.ticket_analyst.enabled", None),
    ("agents.ticket_analyst.include_parent_story", None),
    ("agents.ticket_analyst.use_description", None),
    ("agents.ticket_analyst.cache_ttl_minutes", None),
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

/// Where code, tickets or credentials go, or which program runs with them: admin-only (the settings use
/// cases refuse them too). `jira.mcp.env_passthrough` is admin-only as well but classified as a credential.
/// `Sensitivity::Endpoint` / `McpCommand` stay for proposals stored before this rule.
const ADMIN_ONLY_FIELDS: &[&str] =
    &["llm.base_url", "jira.base_url", "jira.mcp.command", "figma.api_base_url", "figma.mcp_url"];

/// May be turned off by the assistant, never on (admin-only: `QGATE_RUN_REPO_CHECKS`).
pub const RUN_REPO_CHECKS_FIELD: &str = "watch.run_repo_checks";
pub const ADMIN_ONLY_REASON: &str =
    "admin-only: set via environment (qgate.env) and restart qgate; it cannot be changed over HTTP";

/// Every existing leaf under these sections is settable (review-file output).
const SETTABLE_SECTIONS: &[&str] = &["output."];

/// Path fragments that name credentials or pass them on.
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

/// The settable leaf paths present in `settings`, for the model's context.
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
        assert_eq!(classify("llm.base_url"), FieldAccess::AdminOnly);
        assert_eq!(classify("jira.mcp.command"), FieldAccess::AdminOnly);
        assert_eq!(classify("figma.mcp_url"), FieldAccess::AdminOnly);
        assert_eq!(classify("watch.run_repo_checks"), FieldAccess::Settable(Some(Sensitivity::RepoChecks)));
        assert_eq!(classify("output.formats"), FieldAccess::Settable(None));
        assert_eq!(classify("secrets.anthropic_api_key"), FieldAccess::Credential);
        assert_eq!(classify("jira.mcp.env_passthrough"), FieldAccess::Credential);
        assert_eq!(classify("figma.token"), FieldAccess::Credential);
        assert_eq!(classify("dashboard.allowed_hosts"), FieldAccess::NotAllowed);
        assert_eq!(classify("jira.email"), FieldAccess::NotAllowed);
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
