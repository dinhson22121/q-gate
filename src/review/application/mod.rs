pub mod agent_ports;
pub mod batching;
pub mod caching_reviewer;
pub mod feedback_identity;
pub mod ports;
pub mod preflight;
pub mod redaction;
pub mod retry_criteria;
pub mod review_changes;
pub mod run_agents;
pub mod supplementary_checks;
pub mod supplementary_ports;
pub mod work_in_progress;

pub use crate::review::domain::agent_results::{
    AgentReport, DesignMismatch, DesignMismatchKind, RuleAssessment, RuleComplianceStatus, RuleViolation, TestEvidence,
    TestResult, TestScenario, TestVerdict, TicketBrief, TicketContext,
};
pub use crate::review::domain::callers::{CallerContext, CallerSite};
pub use crate::review::domain::checks::{CheckOutcome, CheckResult, CheckSpec};
pub use crate::review::domain::feedback::{FeedbackMark, FindingFeedback};
pub use crate::review::domain::finding::{Category, Confidence, Finding, PreventionRule, Severity};
pub use crate::review::domain::gate::{GateDecision, GatePolicy, UnverifiablePolicy};
pub use crate::review::domain::report::{ReviewReport, SkipReason, SkippedFile, TokenUsage};
pub use crate::review::domain::reproduce::{Demotion, Reproduce};
pub use crate::review::domain::requirements::{
    AcceptanceCriterion, CriterionAssessment, CriterionStatus, EvidenceRef, HumanInput, HumanMark, Requirements,
    RequirementsReport, apply_human_marks,
};
pub use crate::review::domain::unverifiable::{
    Unverifiable, UnverifiableReason, UnverifiableSubject, collect_unverifiable,
};
pub use caching_reviewer::CachingCodeReviewer;
pub use feedback_identity::{FindingIdentity, finding_identity};
pub use ports::HumanVerifications;
pub use preflight::{AgentPlan, ModelPreflight, ProbeFailure, ReviewPreflight};
pub use review_changes::{NoProgress, ReviewChanges, ReviewError, ReviewProgress, ReviewSettings};
pub use run_agents::{AgentRun, AgentSuite};
pub use supplementary_checks::{RunSupplementaryChecks, SupplementarySettings};
pub use supplementary_ports::{
    CommitChange, CommitChanges, ExportedSnapshot, QualityChecks, QualityRequest, RegressionChecks, RegressionRequest,
    SnapshotExporter, ToolOutcome,
};
