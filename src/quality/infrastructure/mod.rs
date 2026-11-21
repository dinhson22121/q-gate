mod depguard;
mod gitleaks;
mod govulncheck;
mod osv_scanner;
mod runner;
mod toolchain;

use std::sync::Arc;

pub use depguard::DepguardCheck;
pub use gitleaks::GitleaksCheck;
pub use govulncheck::GovulncheckCheck;
pub use osv_scanner::OsvScannerCheck;
pub use toolchain::ToolchainPaths;

use crate::quality::application::{QualityCheck, QualityConfig};
use crate::sandbox::application::ExecBackend;
use runner::ToolRunner;

pub fn default_checks(backend: Arc<dyn ExecBackend>, config: &QualityConfig) -> Vec<Arc<dyn QualityCheck>> {
    let runner = ToolRunner::new(backend, config.timeout_secs);
    let paths = &config.toolchains;
    vec![
        Arc::new(GitleaksCheck::new(runner.clone(), paths.gitleaks.clone())),
        Arc::new(GovulncheckCheck::new(runner.clone(), paths.govulncheck.clone())),
        Arc::new(OsvScannerCheck::new(runner.clone(), paths.osv_scanner.clone())),
        Arc::new(DepguardCheck::new(runner, paths.golangci_lint.clone(), config.boundaries.clone())),
    ]
}
