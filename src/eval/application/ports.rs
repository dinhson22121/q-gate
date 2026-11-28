use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;

use crate::eval::domain::case::{CaseError, GoldenCase};
use crate::eval::domain::run::EvalRun;
use crate::history::application::RunProvenance;
use crate::review::application::{Category, Requirements, ReviewReport, Severity, TicketContext};

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvalError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    NotFound(String),
    #[error(transparent)]
    Case(#[from] CaseError),
    #[error("case workspace failed: {0}")]
    Workspace(String),
    #[error("review of case '{case_id}' (run {repetition}) failed: {message}")]
    Review { case_id: String, repetition: u32, message: String },
    #[error("eval storage failed: {0}")]
    Storage(String),
    #[error("{0}")]
    Runner(String),
    #[error("import failed: {0}")]
    Import(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedCase {
    pub case: GoldenCase,
    pub base_dir: PathBuf,
    pub patch: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCase {
    pub case: GoldenCase,
    pub base_files: Vec<BaseFile>,
    pub patch: String,
}

pub trait CaseRepository: Send + Sync {
    fn ids(&self) -> Result<Vec<String>, EvalError>;
    fn load(&self, id: &str) -> Result<LoadedCase, EvalError>;
    fn write(&self, case: &NewCase) -> Result<(), EvalError>;
}

pub struct MaterializedRepo {
    pub root: PathBuf,
    pub base: String,
    pub head: String,
    pub lease: Box<dyn Send + Sync>,
}

pub trait CaseWorkspace: Send + Sync {
    fn materialize<'a>(&'a self, case: &'a LoadedCase) -> BoxFuture<'a, Result<MaterializedRepo, EvalError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewJob {
    pub repo: PathBuf,
    pub base: String,
    pub head: String,
    pub requirements: Option<Requirements>,
    pub ticket: Option<TicketContext>,
    pub with_tests: bool,
}

pub trait ReviewRunner: Send + Sync {
    fn run<'a>(&'a self, job: &'a ReviewJob) -> BoxFuture<'a, Result<ReviewReport, EvalError>>;
}

pub trait EvalStore: Send + Sync {
    fn save(&self, run: &EvalRun) -> Result<i64, EvalError>;
    fn get(&self, id: i64) -> Result<Option<EvalRun>, EvalError>;
    fn latest(&self) -> Result<Option<EvalRun>, EvalError>;
    fn previous_to(&self, id: i64) -> Result<Option<EvalRun>, EvalError>;
    fn latest_for(&self, provenance: &RunProvenance) -> Result<Option<EvalRun>, EvalError>;
    fn list(&self, limit: u32) -> Result<Vec<EvalRun>, EvalError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingJudgment {
    Accepted,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFinding {
    pub file: String,
    pub line: u32,
    pub category: Category,
    pub severity: Severity,
    pub judgment: Option<FindingJudgment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCriterion {
    pub text: String,
    pub human_verified_met: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportTicket {
    pub key: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportableRun {
    pub run_id: i64,
    pub repo_path: PathBuf,
    pub commit: String,
    pub ticket: Option<ImportTicket>,
    pub criteria: Vec<ImportCriterion>,
    pub findings: Vec<ImportFinding>,
}

pub trait RunSource: Send + Sync {
    fn load(&self, run_id: i64) -> Result<ImportableRun, EvalError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedChange {
    pub base_files: Vec<BaseFile>,
    pub patch: String,
}

pub trait ChangeExporter: Send + Sync {
    fn export<'a>(
        &'a self,
        repo: &'a std::path::Path,
        commit: &'a str,
        base_branch: &'a str,
    ) -> BoxFuture<'a, Result<ExportedChange, EvalError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoModel {
    pub repo: String,
    pub model: String,
}

pub trait RepoModels: Send + Sync {
    fn list<'a>(&'a self) -> BoxFuture<'a, Vec<RepoModel>>;
}

pub trait CurrentProvenance: Send + Sync {
    fn current<'a>(&'a self) -> BoxFuture<'a, RunProvenance>;
}
