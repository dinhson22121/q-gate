pub mod baseline;
pub mod import;
pub mod ports;
pub mod queries;
pub mod run_eval;

#[cfg(test)]
pub(crate) mod test_support;

pub use crate::eval::domain::case::{GoldenCase, parse_case, render_case};
pub use crate::eval::domain::compare::{Comparison, DEFAULT_MAX_DROP, compare};
pub use crate::eval::domain::run::EvalRun;
pub use baseline::{BaselineRef, CompareToBaseline};
pub use import::{ImportRun, ImportedCase};
pub use ports::{
    BaseFile, CaseRepository, CaseWorkspace, ChangeExporter, Clock, CurrentProvenance, EvalError, EvalStore,
    ExportedChange, FindingJudgment, ImportCriterion, ImportFinding, ImportTicket, ImportableRun, LoadedCase,
    MaterializedRepo, NewCase, RepoModel, RepoModels, ReviewJob, ReviewRunner, RunSource,
};
pub use queries::{EvalStatus, GetEvalRun, ListEvalRuns, ModelCoverage, RunDetail, RunEntry, StatusView};
pub use run_eval::{EvalOptions, EvalProgress, MAX_RUNS, NoEvalProgress, RunEval};
