//! Ports implemented by infrastructure adapters (and by fakes in tests).

use std::path::Path;

use futures::future::BoxFuture;

use crate::impact::domain::{GoPackage, TestRun};

#[derive(Debug, thiserror::Error)]
pub enum ImpactError {
    /// `go` is missing or the runner cannot be reached; nothing was executed.
    #[error("go toolchain unavailable: {0}")]
    ToolchainUnavailable(String),
    #[error("{0}")]
    Failed(String),
}

/// Which tests to run in one package; an empty `tests` list runs the whole package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSelection {
    pub package: String,
    /// Top-level test names (a subtest re-runs through its parent).
    pub tests: Vec<String>,
}

/// The Go toolchain on a snapshot directory. Implementations run everything through the sandbox.
pub trait GoToolchain: Send + Sync {
    /// True when `root` holds a Go module (cheap, no command run).
    fn is_go_module(&self, root: &Path) -> bool;

    /// Every package of the module at `root`, with the imports of its code and tests.
    fn list_packages<'a>(&'a self, root: &'a Path) -> BoxFuture<'a, Result<Vec<GoPackage>, ImpactError>>;

    /// Runs the existing tests, uncached (`-count=1`), and returns per-test results. A failing test
    /// is a result, not an error.
    fn run_tests<'a>(
        &'a self,
        root: &'a Path,
        selections: &'a [TestSelection],
    ) -> BoxFuture<'a, Result<TestRun, ImpactError>>;
}

/// Identifies one base-side test run. The base sha never changes, so the same key always yields the
/// same results: this is the seam for caching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRunKey {
    pub base_sha: String,
    /// Sorted package import paths that were run.
    pub packages: Vec<String>,
}

impl BaseRunKey {
    /// `(label, content)` pairs ready for `ContentKey::with_inputs` (namespace e.g. `impact.base-tests`).
    pub fn content_inputs(&self) -> Vec<(String, String)> {
        vec![("base_sha".into(), self.base_sha.clone()), ("packages".into(), self.packages.join("\n"))]
    }
}

/// Optional store for base-side results; the lead backs it with the `cache` context.
pub trait BaseRunStore: Send + Sync {
    fn get(&self, key: &BaseRunKey) -> Option<TestRun>;
    fn put(&self, key: &BaseRunKey, run: &TestRun);
}
