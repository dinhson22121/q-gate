pub mod cli_loop;
pub mod mcp_bridge;
pub mod process;
pub mod workspace;

use std::sync::Arc;

use crate::sandbox::application::ExecBackend;
use crate::testing::application::agent::{ScenarioTestAgent, TestAgentSettings};
use crate::testing::application::ports::ScenarioSnapshots;

pub use cli_loop::CliToolLoop;
pub use process::ExecTestRunner;
pub use workspace::SnapshotWorkspace;

pub type ClaudeTestAgent = ScenarioTestAgent<CliToolLoop, ExecTestRunner, SnapshotWorkspace>;

impl ScenarioTestAgent<CliToolLoop, ExecTestRunner, SnapshotWorkspace> {
    pub fn new(
        driver: Arc<CliToolLoop>,
        config: TestAgentSettings,
        backend: Arc<dyn ExecBackend>,
        snapshots: Arc<dyn ScenarioSnapshots>,
    ) -> Self {
        Self::with_ports(driver, ExecTestRunner::new(backend), snapshots, config)
    }
}
