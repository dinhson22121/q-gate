//! A policy proposal: what the model suggested (`ProposalDraft`, untrusted), what survived validation
//! (`ProposalContent`), and the persisted record with its lifecycle (`Proposal`).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::fields::{ADMIN_ONLY_REASON, FieldAccess, RUN_REPO_CHECKS_FIELD, Sensitivity, WEBHOOK_FIELD, classify};
use super::gate::{GateField, gate_value, loosens_gate, loosens_setting, with_gate_value};
use super::json::{get, same_kind};
use crate::review::application::GatePolicy;

pub const MAX_DESCRIPTION_CHARS: usize = 8_000;
/// Items kept per list of a proposal; more are dropped (and reported).
pub const MAX_ITEMS: usize = 20;
const MAX_TITLE_CHARS: usize = 160;
const MAX_TEXT_CHARS: usize = 2_000;
const MAX_GLOBS: usize = 10;
const MAX_CHECK_TIMEOUT_SECS: u64 = 6 * 60 * 60;
/// Proposals older than this can no longer be approved.
pub const PROPOSAL_TTL_HOURS: i64 = 24;

// ---- What the model returns (untrusted; every field is re-validated) ----

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProposalDraft {
    pub summary: String,
    pub settings_changes: Vec<DraftSetting>,
    pub rule_changes: Vec<DraftRule>,
    pub gate_changes: Vec<DraftGate>,
    pub check_suggestions: Vec<DraftCheck>,
    /// Parts of the description that cannot be automated.
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftSetting {
    pub path: String,
    /// The new value as JSON text (`"block"`, `true`, `["copy"]`).
    pub value_json: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    #[default]
    Add,
    Edit,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftRule {
    pub action: RuleAction,
    /// The edited rule's id; empty for `add`.
    pub rule_id: String,
    pub title: String,
    pub description: String,
    pub scope_globs: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftGate {
    pub field: String,
    pub value_json: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftCheck {
    pub name: String,
    pub command: Vec<String>,
    pub required: bool,
    pub timeout_secs: u64,
    pub reason: String,
}

// ---- Validated content ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingChange {
    pub path: String,
    /// Current effective value (the webhook URL is never shown: `null`).
    pub old: Value,
    pub new: Value,
    pub reason: String,
    pub sensitivity: Vec<Sensitivity>,
    pub restart_required: bool,
}

impl SettingChange {
    pub fn is_sensitive(&self) -> bool {
        !self.sensitivity.is_empty()
    }
}

/// An existing rule the model may edit, as shown to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistingRule {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub scope_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleChange {
    pub action: RuleAction,
    /// For an edit: the rule's id and title when the proposal was made (approval refuses if it changed).
    pub replaces: Option<(String, String)>,
    pub title: String,
    pub description: String,
    pub scope_globs: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateChange {
    pub field: GateField,
    pub old: Value,
    pub new: Value,
    pub reason: String,
    pub loosening: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSuggestion {
    pub name: String,
    pub command: Vec<String>,
    pub required: bool,
    pub timeout_secs: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedItem {
    /// `setting`, `rule`, `gate` or `check`.
    pub kind: String,
    pub subject: String,
    pub reason: String,
}

impl RejectedItem {
    pub fn new(kind: &str, subject: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { kind: kind.into(), subject: clip(&subject.into(), MAX_TITLE_CHARS), reason: reason.into() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProposalContent {
    pub summary: String,
    pub settings: Vec<SettingChange>,
    pub rules: Vec<RuleChange>,
    pub gate: Vec<GateChange>,
    pub checks: Vec<CheckSuggestion>,
    pub rejected: Vec<RejectedItem>,
    pub unsupported: Vec<String>,
}

impl ProposalContent {
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.rules.is_empty() && self.gate.is_empty() && self.checks.is_empty()
    }
}

/// What a draft is validated against.
pub struct ProposalContext<'a> {
    /// Effective server settings as JSON, the webhook URL masked as "".
    pub settings: &'a Value,
    /// Fields set by environment variables (cannot change here).
    pub locked: &'a [String],
    pub restart_required: &'a [String],
    /// The repository the proposal is for, if one was chosen.
    pub repo: Option<&'a str>,
    pub rules: &'a [ExistingRule],
    /// The repository's current gate (`.qgate.toml`), when a repository was chosen.
    pub repo_gate: Option<&'a GatePolicy>,
    pub existing_checks: &'a [String],
}

pub(crate) fn clip(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &trimmed[..cut]),
        None => trimmed.to_string(),
    }
}

fn parse_json(raw: &str) -> Result<Value, String> {
    serde_json::from_str(raw.trim()).map_err(|_| "the value is not valid JSON".to_string())
}

/// Validates the model's draft. Never fails: anything unusable becomes a [`RejectedItem`] with the reason.
pub fn validate_draft(draft: &ProposalDraft, ctx: &ProposalContext<'_>) -> ProposalContent {
    let mut rejected = Vec::new();
    let settings = validate_settings(&draft.settings_changes, ctx, &mut rejected);
    let (rules, gate, checks) = match ctx.repo {
        Some(_) => (
            validate_rules(&draft.rule_changes, ctx.rules, &mut rejected),
            validate_gate(&draft.gate_changes, ctx.repo_gate.cloned().unwrap_or_default(), &mut rejected),
            validate_checks(&draft.check_suggestions, ctx.existing_checks, &mut rejected),
        ),
        None => {
            let need_repo = "pick a repository to change its rules, gate or checks";
            for rule in &draft.rule_changes {
                rejected.push(RejectedItem::new("rule", &rule.title, need_repo));
            }
            for gate in &draft.gate_changes {
                rejected.push(RejectedItem::new("gate", &gate.field, need_repo));
            }
            for check in &draft.check_suggestions {
                rejected.push(RejectedItem::new("check", &check.name, need_repo));
            }
            (Vec::new(), Vec::new(), Vec::new())
        }
    };
    ProposalContent {
        summary: clip(&draft.summary, MAX_TEXT_CHARS),
        settings,
        rules,
        gate,
        checks,
        rejected,
        unsupported: draft.unsupported.iter().take(MAX_ITEMS).map(|u| clip(u, MAX_TEXT_CHARS)).collect(),
    }
}

fn too_many(kind: &str, count: usize, rejected: &mut Vec<RejectedItem>) {
    if count > MAX_ITEMS {
        rejected.push(RejectedItem::new(
            kind,
            format!("{} more", count - MAX_ITEMS),
            "too many changes in one proposal",
        ));
    }
}

fn validate_settings(
    drafts: &[DraftSetting],
    ctx: &ProposalContext<'_>,
    rejected: &mut Vec<RejectedItem>,
) -> Vec<SettingChange> {
    too_many("setting", drafts.len(), rejected);
    let mut out: Vec<SettingChange> = Vec::new();
    for draft in drafts.iter().take(MAX_ITEMS) {
        let path = draft.path.trim();
        match validate_setting(path, draft, ctx, &out) {
            Ok(Some(change)) => out.push(change),
            Ok(None) => {}
            Err(reason) => rejected.push(RejectedItem::new("setting", path, reason)),
        }
    }
    out
}

/// `Ok(None)`: the value is already in effect (nothing to do).
fn validate_setting(
    path: &str,
    draft: &DraftSetting,
    ctx: &ProposalContext<'_>,
    accepted: &[SettingChange],
) -> Result<Option<SettingChange>, String> {
    let declared = match classify(path) {
        FieldAccess::Credential => {
            return Err("credentials can never be changed by the assistant; use the Settings page".into());
        }
        FieldAccess::NotAllowed => return Err("this setting cannot be changed by the assistant".into()),
        FieldAccess::AdminOnly => return Err(ADMIN_ONLY_REASON.into()),
        FieldAccess::Settable(sensitivity) => sensitivity,
    };
    let current = get(ctx.settings, path).ok_or("unknown setting")?;
    if ctx.locked.iter().any(|l| l == path) {
        return Err("set by an environment variable; change it in qgate.env and restart qgate".into());
    }
    if accepted.iter().any(|c| c.path == path) {
        return Err("changed twice in one proposal".into());
    }
    let new = parse_json(&draft.value_json)?;
    if !same_kind(current, &new) {
        return Err(format!("expected a value like {current}"));
    }
    let is_webhook = path == WEBHOOK_FIELD;
    // The webhook URL carries a token and is write-only: it can be cleared here but only set on the Settings page.
    if is_webhook && new != "" {
        return Err(
            "webhook URLs contain a token: set them on the Settings page; the assistant can only clear one".into()
        );
    }
    if !is_webhook && current == &new {
        return Ok(None);
    }
    if path == RUN_REPO_CHECKS_FIELD && new == true {
        return Err(ADMIN_ONLY_REASON.into());
    }
    let mut sensitivity: Vec<Sensitivity> = declared.into_iter().collect();
    if loosens_setting(path, current, &new) {
        sensitivity.push(Sensitivity::GateLoosening);
    }
    Ok(Some(SettingChange {
        path: path.to_string(),
        old: if is_webhook { Value::Null } else { current.clone() },
        new,
        reason: clip(&draft.reason, MAX_TEXT_CHARS),
        sensitivity,
        restart_required: ctx.restart_required.iter().any(|r| r == path),
    }))
}

fn valid_glob(glob: &str) -> bool {
    !glob.trim().is_empty() && globset::Glob::new(glob).is_ok()
}

fn validate_rules(
    drafts: &[DraftRule],
    existing: &[ExistingRule],
    rejected: &mut Vec<RejectedItem>,
) -> Vec<RuleChange> {
    too_many("rule", drafts.len(), rejected);
    let mut out: Vec<RuleChange> = Vec::new();
    for draft in drafts.iter().take(MAX_ITEMS) {
        match validate_rule(draft, existing, &out) {
            Ok(change) => out.push(change),
            Err(reason) => rejected.push(RejectedItem::new("rule", &draft.title, reason)),
        }
    }
    out
}

fn validate_rule(draft: &DraftRule, existing: &[ExistingRule], accepted: &[RuleChange]) -> Result<RuleChange, String> {
    let title = draft.title.trim();
    if title.is_empty() || draft.description.trim().is_empty() {
        return Err("a rule needs a title and a description".into());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("the title is longer than {MAX_TITLE_CHARS} characters"));
    }
    if draft.scope_globs.len() > MAX_GLOBS || !draft.scope_globs.iter().all(|g| valid_glob(g)) {
        return Err(format!("scope must be at most {MAX_GLOBS} valid glob patterns"));
    }
    if accepted.iter().any(|r| r.title.eq_ignore_ascii_case(title)) {
        return Err("the same rule appears twice".into());
    }
    let replaces = match draft.action {
        RuleAction::Add => None,
        RuleAction::Edit => {
            let rule = existing.iter().find(|r| r.id == draft.rule_id.trim()).ok_or("edits an unknown rule id")?;
            Some((rule.id.clone(), rule.title.clone()))
        }
    };
    Ok(RuleChange {
        action: draft.action,
        replaces,
        title: title.to_string(),
        description: clip(&draft.description, MAX_TEXT_CHARS),
        scope_globs: draft.scope_globs.iter().map(|g| g.trim().to_string()).collect(),
        reason: clip(&draft.reason, MAX_TEXT_CHARS),
    })
}

fn validate_gate(drafts: &[DraftGate], current: GatePolicy, rejected: &mut Vec<RejectedItem>) -> Vec<GateChange> {
    too_many("gate", drafts.len(), rejected);
    let mut out: Vec<GateChange> = Vec::new();
    for draft in drafts.iter().take(MAX_ITEMS) {
        let result =
            GateField::parse(draft.field.trim()).ok_or_else(|| "unknown [gate] field".to_string()).and_then(|field| {
                if out.iter().any(|c| c.field == field) {
                    return Err("changed twice in one proposal".into());
                }
                let value = parse_json(&draft.value_json)?;
                let next = with_gate_value(&current, field, &value)?;
                Ok((field, next))
            });
        match result {
            Ok((field, next)) => {
                let (old, new) = (gate_value(&current, field), gate_value(&next, field));
                if old != new {
                    let loosening = loosens_gate(field, &current, &next);
                    out.push(GateChange { field, old, new, reason: clip(&draft.reason, MAX_TEXT_CHARS), loosening });
                }
            }
            Err(reason) => rejected.push(RejectedItem::new("gate", &draft.field, reason)),
        }
    }
    out
}

fn validate_checks(
    drafts: &[DraftCheck],
    existing: &[String],
    rejected: &mut Vec<RejectedItem>,
) -> Vec<CheckSuggestion> {
    too_many("check", drafts.len(), rejected);
    let mut out: Vec<CheckSuggestion> = Vec::new();
    for draft in drafts.iter().take(MAX_ITEMS) {
        let name = draft.name.trim();
        let reason = if name.is_empty() {
            Some("a check needs a name".to_string())
        } else if existing.iter().chain(out.iter().map(|c| &c.name)).any(|n| n == name) {
            Some("a check with this name already exists".into())
        } else if draft.command.first().is_none_or(|program| program.trim().is_empty()) {
            Some("a check needs a command (program and arguments, no shell)".into())
        } else if !(1..=MAX_CHECK_TIMEOUT_SECS).contains(&draft.timeout_secs) {
            Some(format!("timeout_secs must be between 1 and {MAX_CHECK_TIMEOUT_SECS}"))
        } else {
            None
        };
        match reason {
            Some(reason) => rejected.push(RejectedItem::new("check", name, reason)),
            None => out.push(CheckSuggestion {
                name: name.to_string(),
                command: draft.command.clone(),
                required: draft.required,
                timeout_secs: draft.timeout_secs,
                reason: clip(&draft.reason, MAX_TEXT_CHARS),
            }),
        }
    }
    out
}

// ---- Lifecycle ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Draft,
    Applied,
    Rejected,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [Self::Draft, Self::Applied, Self::Rejected].into_iter().find(|s| s.as_str() == raw)
    }
}

/// Status to give rules added on approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleApproval {
    Approved,
    Proposed,
}

/// What happened when a proposal was approved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOutcome {
    /// The settings version written, if settings changed.
    pub settings_version: Option<i64>,
    pub settings_applied: Vec<String>,
    pub restart_required: Vec<String>,
    pub rules_written: Vec<String>,
    /// Steps that failed after others were applied.
    pub errors: Vec<String>,
}

/// A stored proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// 0 before it is stored.
    pub id: i64,
    pub status: ProposalStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub description: String,
    pub repo: Option<String>,
    /// Settings version the proposal was validated against; approval requires it to still be current.
    pub settings_version: i64,
    pub draft: ProposalDraft,
    pub content: ProposalContent,
    pub outcome: Option<ApplyOutcome>,
}

impl Proposal {
    pub fn new(
        description: String,
        repo: Option<String>,
        settings_version: i64,
        draft: ProposalDraft,
        content: ProposalContent,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: 0,
            status: ProposalStatus::Draft,
            created_at: now,
            expires_at: now + Duration::hours(PROPOSAL_TTL_HOURS),
            description,
            repo,
            settings_version,
            draft,
            content,
            outcome: None,
        }
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// The user's choice on the preview page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// Indexes into `content.settings`.
    pub settings: Vec<usize>,
    /// Indexes into `content.rules` with the status each gets.
    pub rules: Vec<(usize, RuleApproval)>,
    /// The user ticked the separate confirmation for sensitive changes.
    pub confirm_sensitive: bool,
}

/// Why a proposal cannot be approved as asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalError {
    #[error("this proposal is {0}; only a draft can be approved")]
    NotDraft(&'static str),
    #[error("this proposal expired; describe the change again or re-preview it")]
    Expired,
    #[error(
        "the settings changed since this proposal was made (version {proposed} → {current}); re-preview it before approving"
    )]
    Stale { proposed: i64, current: i64 },
    #[error("nothing selected to apply")]
    NothingSelected,
    #[error("unknown {0} item {1}")]
    UnknownItem(&'static str, usize),
    #[error("these changes need the separate confirmation: {0}")]
    NeedsConfirmation(String),
}

/// The selected changes, checked and ready to apply.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalPlan {
    pub settings: Vec<SettingChange>,
    pub rules: Vec<(RuleChange, RuleApproval)>,
}

impl Proposal {
    pub fn plan(
        &self,
        request: &ApprovalRequest,
        current_version: i64,
        now: DateTime<Utc>,
    ) -> Result<ApprovalPlan, ApprovalError> {
        if self.status != ProposalStatus::Draft {
            return Err(ApprovalError::NotDraft(self.status.as_str()));
        }
        if self.is_expired(now) {
            return Err(ApprovalError::Expired);
        }
        if current_version != self.settings_version {
            return Err(ApprovalError::Stale { proposed: self.settings_version, current: current_version });
        }
        let mut settings = Vec::new();
        for &index in &dedup(request.settings.iter().copied()) {
            settings
                .push(self.content.settings.get(index).cloned().ok_or(ApprovalError::UnknownItem("setting", index))?);
        }
        let mut rules = Vec::new();
        for &index in &dedup(request.rules.iter().map(|(i, _)| *i)) {
            let change = self.content.rules.get(index).cloned().ok_or(ApprovalError::UnknownItem("rule", index))?;
            let status = request.rules.iter().find(|(i, _)| *i == index).map_or(RuleApproval::Proposed, |(_, s)| *s);
            rules.push((change, status));
        }
        if settings.is_empty() && rules.is_empty() {
            return Err(ApprovalError::NothingSelected);
        }
        let unconfirmed: Vec<&str> =
            settings.iter().filter(|c| c.is_sensitive()).map(|c| c.path.as_str()).collect::<Vec<_>>();
        if !unconfirmed.is_empty() && !request.confirm_sensitive {
            return Err(ApprovalError::NeedsConfirmation(unconfirmed.join(", ")));
        }
        Ok(ApprovalPlan { settings, rules })
    }
}

fn dedup(indexes: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut out: Vec<usize> = indexes.collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
#[path = "proposal_tests.rs"]
mod tests;
