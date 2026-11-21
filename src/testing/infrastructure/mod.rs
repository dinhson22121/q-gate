//! Adapters: snapshot file system, test runner (via the sandbox backend) and the Claude tool loop.

pub mod process;
pub mod workspace;

use std::future::Future;
use std::sync::Arc;

use crate::llm::{ClaudeClient, ContentPart, LlmError, LoopLimits, LoopOutcome, SystemBlock, ToolHandler, ToolSpec};
use crate::sandbox::application::ExecBackend;
use crate::testing::application::agent::{ScenarioTestAgent, TestAgentSettings};
use crate::testing::application::ports::{ScenarioSnapshots, ToolLoop};

pub use process::ExecTestRunner;
pub use workspace::SnapshotWorkspace;

impl ToolLoop for ClaudeClient {
    fn drive<H: ToolHandler>(
        &self,
        system: &[SystemBlock],
        user: Vec<ContentPart>,
        tools: &[ToolSpec],
        handler: &H,
        limits: LoopLimits,
    ) -> impl Future<Output = Result<LoopOutcome, LlmError>> + Send {
        self.run_tools(system, user, tools, handler, limits)
    }
}

/// The production test agent: Claude + the sandbox's execution backend + one snapshot per scenario.
pub type ClaudeTestAgent = ScenarioTestAgent<ClaudeClient, ExecTestRunner, SnapshotWorkspace>;

impl ScenarioTestAgent<ClaudeClient, ExecTestRunner, SnapshotWorkspace> {
    pub fn new(
        client: Arc<ClaudeClient>,
        config: TestAgentSettings,
        backend: Arc<dyn ExecBackend>,
        snapshots: Arc<dyn ScenarioSnapshots>,
    ) -> Self {
        Self::with_ports(client, ExecTestRunner::new(backend), snapshots, config)
    }
}
