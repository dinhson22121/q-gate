//! Use case: run the enabled deterministic checks against a commit snapshot and grade them on the new code.

mod config;
mod ports;
mod run_quality_checks;

pub use config::QualityConfig;
pub use ports::{CheckInput, CheckOutcome, QualityCheck};
pub use run_quality_checks::{QualityReport, QualityRequest, RunQualityChecks};
