use std::path::Path;

use futures::future::BoxFuture;

use crate::impact::domain::{GoPackage, TestRun};

#[derive(Debug, thiserror::Error)]
pub enum ImpactError {
    #[error("go toolchain unavailable: {0}")]
    ToolchainUnavailable(String),
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSelection {
    pub package: String,
    pub tests: Vec<String>,
}

pub trait GoToolchain: Send + Sync {
    fn is_go_module(&self, root: &Path) -> bool;

    fn list_packages<'a>(&'a self, root: &'a Path) -> BoxFuture<'a, Result<Vec<GoPackage>, ImpactError>>;

    fn run_tests<'a>(
        &'a self,
        root: &'a Path,
        selections: &'a [TestSelection],
    ) -> BoxFuture<'a, Result<TestRun, ImpactError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRunKey {
    pub base_sha: String,
    pub packages: Vec<String>,
}

impl BaseRunKey {
    pub fn content_inputs(&self) -> Vec<(String, String)> {
        vec![("base_sha".into(), self.base_sha.clone()), ("packages".into(), self.packages.join("\n"))]
    }
}

pub trait BaseRunStore: Send + Sync {
    fn get(&self, key: &BaseRunKey) -> Option<TestRun>;
    fn put(&self, key: &BaseRunKey, run: &TestRun);
}
