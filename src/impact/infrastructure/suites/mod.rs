mod common;
mod junit;
mod jvm;
mod node;
mod python;
mod rust;

use std::sync::Arc;

use crate::impact::application::TestSuite;
use crate::sandbox::application::ExecBackend;
pub use jvm::{JvmBuild, JvmSuite};
pub use node::{NodeRunner, NodeSuite};
pub use python::PytestSuite;
pub use rust::CargoSuite;

pub fn all_suites(backend: &Arc<dyn ExecBackend>) -> Vec<Arc<dyn TestSuite>> {
    vec![
        Arc::new(JvmSuite::new(JvmBuild::Maven, backend.clone())),
        Arc::new(JvmSuite::new(JvmBuild::Gradle, backend.clone())),
        Arc::new(PytestSuite::new(backend.clone())),
        Arc::new(NodeSuite::new(NodeRunner::Jest, backend.clone())),
        Arc::new(NodeSuite::new(NodeRunner::Vitest, backend.clone())),
        Arc::new(CargoSuite::new(backend.clone())),
    ]
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod live_tests;
