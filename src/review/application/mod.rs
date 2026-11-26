pub mod agent_ports;
pub mod batching;
pub mod caching_reviewer;
pub mod ports;
pub mod preflight;
pub mod redaction;
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
pub use crate::review::domain::finding::{Category, Confidence, Finding, PreventionRule, Severity};
pub use crate::review::domain::gate::GatePolicy;
pub use crate::review::domain::report::{ReviewReport, SkipReason, TokenUsage};
pub use crate::review::domain::reproduce::{Demotion, Reproduce};
pub use crate::review::domain::requirements::{
    AcceptanceCriterion, CriterionStatus, EvidenceRef, Requirements, RequirementsReport,
};
pub use caching_reviewer::CachingCodeReviewer;
pub use preflight::{AgentPlan, ModelPreflight, ProbeFailure, ReviewPreflight};
pub use review_changes::{NoProgress, ReviewChanges, ReviewError, ReviewProgress, ReviewSettings};
pub use run_agents::{AgentRun, AgentSuite};
pub use supplementary_checks::{RunSupplementaryChecks, SupplementarySettings};
pub use supplementary_ports::{
    CommitChange, CommitChanges, ExportedSnapshot, QualityChecks, QualityRequest, RegressionChecks, RegressionRequest,
    SnapshotExporter, ToolOutcome,
};
