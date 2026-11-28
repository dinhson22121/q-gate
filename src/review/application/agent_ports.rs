use std::path::Path;

use futures::future::BoxFuture;

use crate::review::domain::agent_results::{
    DesignMismatch, RuleAssessment, RuleViolation, TestResult, TestScenario, TicketBrief, TicketContext,
};
use crate::review::domain::callers::CallerSite;
use crate::review::domain::change_set::FileChange;
use crate::review::domain::requirements::Requirements;

pub struct AgentInput<'a> {
    pub files: &'a [FileChange],
    pub requirements: Option<&'a Requirements>,
    pub ticket: Option<&'a TicketContext>,
    pub commit: &'a str,
    pub repo_root: &'a Path,
    pub snapshot_root: Option<&'a Path>,
    pub callers: &'a [CallerSite],
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AgentError(pub String);

pub trait TicketAnalyst: Send + Sync {
    fn analyze<'a>(&'a self, input: &'a AgentInput<'a>) -> BoxFuture<'a, Result<TicketBrief, AgentError>>;
}

pub trait TestAgent: Send + Sync {
    fn run<'a>(&'a self, input: &'a AgentInput<'a>, scenario: &'a TestScenario) -> BoxFuture<'a, TestResult>;

    fn environment<'a>(&'a self, _input: &'a AgentInput<'a>) -> BoxFuture<'a, String> {
        Box::pin(async { String::new() })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchitectureReview {
    pub violations: Vec<RuleViolation>,
    pub assessments: Vec<RuleAssessment>,
}

pub trait ArchitectureReviewer: Send + Sync {
    fn review<'a>(
        &'a self,
        input: &'a AgentInput<'a>,
        brief: Option<&'a TicketBrief>,
    ) -> BoxFuture<'a, Result<ArchitectureReview, AgentError>>;
}

pub trait DesignReviewer: Send + Sync {
    fn review<'a>(
        &'a self,
        input: &'a AgentInput<'a>,
        brief: Option<&'a TicketBrief>,
    ) -> BoxFuture<'a, Result<Vec<DesignMismatch>, AgentError>>;
}
