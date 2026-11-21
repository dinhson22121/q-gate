pub mod dry_run;
pub mod fields;
pub mod gate;
pub mod json;
pub mod preview;
pub mod proposal;

pub use dry_run::{DryRunReport, DryRunRow, RunFacts, passes, summarize};
pub use fields::{FieldAccess, Sensitivity, WEBHOOK_FIELD, classify, settable_paths};
pub use gate::{GateField, gate_value, with_gate_value};
pub use preview::{DiffLine, LineKind, PreviewSection, render_preview, repo_patch};
pub use proposal::{
    ApplyOutcome, ApprovalError, ApprovalPlan, ApprovalRequest, CheckSuggestion, DraftCheck, DraftGate, DraftRule,
    DraftSetting, ExistingRule, GateChange, MAX_DESCRIPTION_CHARS, Proposal, ProposalContent, ProposalContext,
    ProposalDraft, ProposalStatus, RejectedItem, RuleAction, RuleApproval, RuleChange, SettingChange, validate_draft,
};
