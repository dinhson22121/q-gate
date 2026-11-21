use std::collections::BTreeSet;

use crate::quality::domain::{ImportBoundary, Language, QualityTool};

pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub struct QualityConfig {
    pub languages: BTreeSet<Language>,
    pub tools: BTreeSet<QualityTool>,
    pub boundaries: Vec<ImportBoundary>,
    pub timeout_secs: u64,
    pub toolchains: crate::quality::infrastructure::ToolchainPaths,
}

impl QualityConfig {
    pub fn new(
        languages: impl IntoIterator<Item = Language>,
        toolchains: crate::quality::infrastructure::ToolchainPaths,
    ) -> Self {
        Self {
            languages: languages.into_iter().collect(),
            tools: QualityTool::ALL.into_iter().collect(),
            boundaries: Vec::new(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            toolchains,
        }
    }

    pub fn with_boundaries(mut self, boundaries: Vec<ImportBoundary>) -> Self {
        self.boundaries = boundaries;
        self
    }

    pub fn with_tools(mut self, tools: impl IntoIterator<Item = QualityTool>) -> Self {
        self.tools = tools.into_iter().collect();
        self
    }
}
