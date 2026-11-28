use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::TimeZone;
use futures::future::BoxFuture;

use super::ports::{
    CaseRepository, CaseWorkspace, Clock, EvalError, EvalStore, LoadedCase, MaterializedRepo, NewCase, ReviewJob,
    ReviewRunner,
};
use crate::eval::domain::case::parse_case;
use crate::eval::domain::fixtures::score;
use crate::eval::domain::run::EvalRun;
use crate::history::application::RunProvenance;
use crate::review::application::{
    Category, CriterionAssessment, CriterionStatus, Requirements, RequirementsReport, ReviewReport, Severity,
};

pub fn provenance() -> RunProvenance {
    RunProvenance { model: "m".into(), claude_cli_version: "1.0".into(), prompt_hash: "h".repeat(64) }
}

pub fn fixed_clock() -> Clock {
    Arc::new(|| chrono::Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap())
}

pub fn sample_run(caught: &[bool]) -> EvalRun {
    EvalRun::new(fixed_clock()(), provenance(), 1, false, vec![score("a", 0, caught, 0)])
}

pub fn report_with(
    findings: &[(&str, u32, Category, Severity)],
    statuses: &[(&str, CriterionStatus)],
    cost: f64,
) -> ReviewReport {
    use crate::review::domain::finding::fixtures::finding;
    let findings = findings
        .iter()
        .map(|(file, line, category, severity)| {
            let mut f = finding(file, *line, *severity, "evidence");
            f.category = *category;
            f
        })
        .collect();
    let requirements = (!statuses.is_empty()).then(|| RequirementsReport {
        requirements: Requirements { key: "KAN-1".into(), title: String::new(), url: None, criteria: Vec::new() },
        assessments: statuses
            .iter()
            .map(|(id, status)| CriterionAssessment {
                criterion_id: (*id).into(),
                status: *status,
                explanation: String::new(),
                evidence: Vec::new(),
                reason: None,
                retried: false,
                human: None,
            })
            .collect(),
    });
    ReviewReport { findings, requirements, estimated_cost_usd: Some(cost), ..ReviewReport::default() }
}

pub struct FakeCases {
    cases: BTreeMap<String, String>,
    pub written: Mutex<Vec<NewCase>>,
}

impl FakeCases {
    pub fn new(cases: &[(&str, &str)]) -> Self {
        Self {
            cases: cases.iter().map(|(id, toml)| (id.to_string(), toml.to_string())).collect(),
            written: Mutex::default(),
        }
    }
}

impl CaseRepository for FakeCases {
    fn ids(&self) -> Result<Vec<String>, EvalError> {
        Ok(self.cases.keys().cloned().collect())
    }

    fn load(&self, id: &str) -> Result<LoadedCase, EvalError> {
        let text = self.cases.get(id).ok_or_else(|| EvalError::NotFound(id.into()))?;
        Ok(LoadedCase {
            case: parse_case(text)?,
            base_dir: PathBuf::from("/base"),
            patch: String::new(),
            content_hash: format!("hash-of-{id}"),
        })
    }

    fn write(&self, case: &NewCase) -> Result<(), EvalError> {
        if self.cases.contains_key(&case.case.id) {
            return Err(EvalError::InvalidInput(format!("case '{}' already exists", case.case.id)));
        }
        self.written.lock().unwrap().push(case.clone());
        Ok(())
    }
}

#[derive(Default)]
pub struct FakeWorkspace {
    count: Mutex<usize>,
}

impl FakeWorkspace {
    pub fn materialized(&self) -> usize {
        *self.count.lock().unwrap()
    }
}

impl CaseWorkspace for FakeWorkspace {
    fn materialize<'a>(&'a self, _case: &'a LoadedCase) -> BoxFuture<'a, Result<MaterializedRepo, EvalError>> {
        *self.count.lock().unwrap() += 1;
        Box::pin(async {
            Ok(MaterializedRepo {
                root: PathBuf::from("/repo"),
                base: "base-sha".into(),
                head: "head-sha".into(),
                lease: Box::new(()),
            })
        })
    }
}

pub struct ScriptedRunner {
    replies: Mutex<std::collections::VecDeque<Result<ReviewReport, EvalError>>>,
    jobs: Mutex<Vec<ReviewJob>>,
}

impl ScriptedRunner {
    pub fn new(replies: Vec<Result<ReviewReport, EvalError>>) -> Self {
        Self { replies: Mutex::new(replies.into()), jobs: Mutex::default() }
    }

    pub fn jobs(&self) -> Vec<ReviewJob> {
        self.jobs.lock().unwrap().clone()
    }
}

impl ReviewRunner for ScriptedRunner {
    fn run<'a>(&'a self, job: &'a ReviewJob) -> BoxFuture<'a, Result<ReviewReport, EvalError>> {
        self.jobs.lock().unwrap().push(job.clone());
        let reply = self.replies.lock().unwrap().pop_front().unwrap_or_else(|| Ok(ReviewReport::default()));
        Box::pin(async move { reply })
    }
}

#[derive(Default)]
pub struct MemoryStore {
    runs: Mutex<Vec<EvalRun>>,
}

impl EvalStore for MemoryStore {
    fn save(&self, run: &EvalRun) -> Result<i64, EvalError> {
        let mut runs = self.runs.lock().unwrap();
        let id = runs.len() as i64 + 1;
        runs.push(EvalRun { id, ..run.clone() });
        Ok(id)
    }

    fn get(&self, id: i64) -> Result<Option<EvalRun>, EvalError> {
        Ok(self.runs.lock().unwrap().iter().find(|r| r.id == id).cloned())
    }

    fn latest(&self) -> Result<Option<EvalRun>, EvalError> {
        Ok(self.runs.lock().unwrap().last().cloned())
    }

    fn latest_for(&self, provenance: &RunProvenance) -> Result<Option<EvalRun>, EvalError> {
        Ok(self.runs.lock().unwrap().iter().rev().find(|r| &r.provenance == provenance).cloned())
    }

    fn previous_to(&self, id: i64) -> Result<Option<EvalRun>, EvalError> {
        Ok(self.runs.lock().unwrap().iter().rev().find(|r| r.id < id).cloned())
    }

    fn list(&self, limit: u32) -> Result<Vec<EvalRun>, EvalError> {
        Ok(self.runs.lock().unwrap().iter().rev().take(limit as usize).cloned().collect())
    }
}
