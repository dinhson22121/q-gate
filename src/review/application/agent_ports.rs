//! Ports for the ticket agents. Dyn-compatible (boxed futures) because each one is optional and
//! configured at runtime.

use std::path::Path;

use futures::future::BoxFuture;

use crate::review::domain::agent_results::{
    DesignMismatch, RuleViolation, TestResult, TestScenario, TicketBrief, TicketContext,
};
use crate::review::domain::change_set::FileChange;
use crate::review::domain::requirements::Requirements;

/// What every agent may look at for one review.
pub struct AgentInput<'a> {
    /// Reviewable changed files (diff + new content).
    pub files: &'a [FileChange],
    pub requirements: Option<&'a Requirements>,
    pub ticket: Option<&'a TicketContext>,
    /// Full sha of the newest reviewed commit.
    pub commit: &'a str,
    /// Repository root (read-only).
    pub repo_root: &'a Path,
    /// Writable copy of the commit's tree, when one was made (test agents need it).
    pub snapshot_root: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AgentError(pub String);

/// Turns criteria + description + design into test scenarios and context for the other agents.
pub trait TicketAnalyst: Send + Sync {
    fn analyze<'a>(&'a self, input: &'a AgentInput<'a>) -> BoxFuture<'a, Result<TicketBrief, AgentError>>;
}

/// Writes and runs one test for one scenario. Never fails: problems become `Inconclusive`.
pub trait TestAgent: Send + Sync {
    fn run<'a>(&'a self, input: &'a AgentInput<'a>, scenario: &'a TestScenario) -> BoxFuture<'a, TestResult>;
}

/// Checks the change against the repository's (cached) rulebook.
pub trait ArchitectureReviewer: Send + Sync {
    fn review<'a>(
        &'a self,
        input: &'a AgentInput<'a>,
        brief: Option<&'a TicketBrief>,
    ) -> BoxFuture<'a, Result<Vec<RuleViolation>, AgentError>>;
}

/// Compares changed UI code with the Figma frames linked from the ticket.
pub trait DesignReviewer: Send + Sync {
    fn review<'a>(
        &'a self,
        input: &'a AgentInput<'a>,
        brief: Option<&'a TicketBrief>,
    ) -> BoxFuture<'a, Result<Vec<DesignMismatch>, AgentError>>;
}
