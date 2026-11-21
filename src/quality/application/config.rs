use std::collections::BTreeSet;

use crate::quality::domain::{ImportBoundary, Language, QualityTool};

/// Longest a single tool may run.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// What to run. Tools that are not installed are skipped with a note regardless of what is enabled here.
#[derive(Debug, Clone)]
pub struct QualityConfig {
    /// Languages the repository's toolchains cover; language-specific tools run only for these.
    pub languages: BTreeSet<Language>,
    pub tools: BTreeSet<QualityTool>,
    /// Import boundaries to enforce (approved Rulebook rules and team rules); none disables depguard.
    pub boundaries: Vec<ImportBoundary>,
    pub timeout_secs: u64,
    /// Where each tool's binary lives (see `ToolchainPaths::discover`).
    pub toolchains: crate::quality::infrastructure::ToolchainPaths,
}

impl QualityConfig {
    /// Every tool enabled for `languages`, no boundaries, default timeout.
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
