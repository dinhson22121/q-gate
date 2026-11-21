use std::collections::HashMap;
use std::sync::Mutex;

use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::ports::{
    FindingContext, GatePolicySource, PolicyError, PolicyModel, PolicyPrompt, ProposalStore, RepoGate, RuleWrite,
    RulebookGateway, RunHistory, SettingsApplied, SettingsGateway, SettingsSnapshot,
};
use crate::policy::domain::json::{get, set};
use crate::policy::domain::{ExistingRule, Proposal, ProposalDraft, ProposalStatus, RunFacts};
use crate::review::application::GatePolicy;

#[derive(Default)]
pub struct FakeModel {
    pub draft: Mutex<ProposalDraft>,
    pub error: Option<PolicyError>,
    pub prompts: Mutex<Vec<PolicyPrompt>>,
}

impl FakeModel {
    pub fn returning(draft: ProposalDraft) -> Self {
        Self { draft: Mutex::new(draft), ..Self::default() }
    }
}

impl PolicyModel for FakeModel {
    fn propose<'a>(&'a self, prompt: &'a PolicyPrompt) -> BoxFuture<'a, Result<ProposalDraft, PolicyError>> {
        Box::pin(async move {
            self.prompts.lock().unwrap().push(prompt.clone());
            match &self.error {
                Some(err) => Err(err.clone()),
                None => Ok(self.draft.lock().unwrap().clone()),
            }
        })
    }
}

pub fn default_settings() -> Value {
    json!({
        "agents": {
            "enabled": true, "max_parallel": 3,
            "tests": {"enabled": true, "on_fail": "warn"},
            "architecture": {"on_violation": "warn"}
        },
        "llm": {"claude_bin": "claude", "timeout_secs": 600},
        "notify": {"webhook_url": ""},
        "watch": {"run_repo_checks": true},
        "dashboard": {"bind": "127.0.0.1:8080"}
    })
}

pub type AppliedChanges = (Vec<(String, Value)>, String);

pub struct FakeSettings {
    pub snapshot: Mutex<SettingsSnapshot>,
    pub applied: Mutex<Vec<AppliedChanges>>,
}

impl Default for FakeSettings {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(SettingsSnapshot {
                version: 3,
                config: default_settings(),
                locked: Vec::new(),
                restart_required: Vec::new(),
                webhook_configured: false,
            }),
            applied: Mutex::default(),
        }
    }
}

impl FakeSettings {
    pub fn bump_version(&self) {
        self.snapshot.lock().unwrap().version += 1;
    }
}

impl SettingsGateway for FakeSettings {
    fn snapshot(&self) -> Result<SettingsSnapshot, PolicyError> {
        Ok(self.snapshot.lock().unwrap().clone())
    }

    fn validate(&self, config: &Value) -> Result<(), String> {
        if get(config, "agents.max_parallel").and_then(Value::as_f64).is_some_and(|b| b > 8.0) {
            return Err("agents.max_parallel: must be at most 8".into());
        }
        if get(config, "agents.tests.on_fail") == Some(&json!("block"))
            && get(config, "agents.tests.enabled") == Some(&json!(false))
        {
            return Err("agents.tests.on_fail: tests are disabled".into());
        }
        Ok(())
    }

    fn apply(
        &self,
        changes: &[(String, Value)],
        changed_by: &str,
        expected_version: i64,
    ) -> Result<SettingsApplied, PolicyError> {
        let mut snapshot = self.snapshot.lock().unwrap();
        if snapshot.version != expected_version {
            return Err(PolicyError::Conflict("settings changed".into()));
        }
        for (path, value) in changes {
            set(&mut snapshot.config, path, value.clone());
        }
        snapshot.version += 1;
        self.applied.lock().unwrap().push((changes.to_vec(), changed_by.to_string()));
        Ok(SettingsApplied {
            version: Some(snapshot.version),
            changed_fields: changes.iter().map(|(p, _)| p.clone()).collect(),
            restart_required: Vec::new(),
        })
    }
}

#[derive(Default)]
pub struct FakeRulebooks {
    pub rules: Mutex<HashMap<String, Vec<ExistingRule>>>,
    pub writes: Mutex<Vec<(String, Vec<RuleWrite>)>>,
    pub fail: bool,
}

impl RulebookGateway for FakeRulebooks {
    fn rules<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<Vec<ExistingRule>, PolicyError>> {
        Box::pin(async move { Ok(self.rules.lock().unwrap().get(repo).cloned().unwrap_or_default()) })
    }

    fn repos(&self) -> BoxFuture<'_, Result<Vec<String>, PolicyError>> {
        Box::pin(async move { Ok(self.rules.lock().unwrap().keys().cloned().collect()) })
    }

    fn write<'a>(&'a self, repo: &'a str, rules: Vec<RuleWrite>) -> BoxFuture<'a, Result<Vec<String>, PolicyError>> {
        Box::pin(async move {
            if self.fail {
                return Err(PolicyError::Conflict("rule R1 changed".into()));
            }
            let titles = rules.iter().map(|r| r.title.clone()).collect();
            self.writes.lock().unwrap().push((repo.to_string(), rules));
            Ok(titles)
        })
    }
}

#[derive(Default)]
pub struct FakeHistory {
    pub runs: Vec<RunFacts>,
    pub skipped: usize,
    pub findings: HashMap<(i64, usize), FindingContext>,
}

impl RunHistory for FakeHistory {
    fn recent(&self, limit: u32) -> Result<(Vec<RunFacts>, usize), PolicyError> {
        Ok((self.runs.iter().take(limit as usize).cloned().collect(), self.skipped))
    }

    fn finding(&self, run_id: i64, index: usize) -> Result<Option<FindingContext>, PolicyError> {
        Ok(self.findings.get(&(run_id, index)).cloned())
    }

    fn repo_paths(&self) -> Result<Vec<String>, PolicyError> {
        Ok(self.runs.iter().map(|r| r.repo_path.clone()).collect())
    }
}

#[derive(Default)]
pub struct FakeGates {
    pub gates: HashMap<String, RepoGate>,
}

impl GatePolicySource for FakeGates {
    fn repo_gate(&self, repo_path: &str) -> RepoGate {
        self.gates.get(repo_path).cloned().unwrap_or_default()
    }

    fn with_server(&self, policy: GatePolicy, settings: &Value) -> GatePolicy {
        GatePolicy { fail_on_test_failure: get(settings, "agents.tests.on_fail") == Some(&json!("block")), ..policy }
    }
}

#[derive(Default)]
pub struct MemoryProposals {
    pub rows: Mutex<Vec<Proposal>>,
}

impl ProposalStore for MemoryProposals {
    fn insert(&self, proposal: &Proposal) -> Result<i64, PolicyError> {
        let mut rows = self.rows.lock().unwrap();
        let id = rows.len() as i64 + 1;
        rows.push(Proposal { id, ..proposal.clone() });
        Ok(id)
    }

    fn get(&self, id: i64) -> Result<Option<Proposal>, PolicyError> {
        Ok(self.rows.lock().unwrap().iter().find(|p| p.id == id).cloned())
    }

    fn transition(&self, proposal: &Proposal, from: ProposalStatus) -> Result<bool, PolicyError> {
        let mut rows = self.rows.lock().unwrap();
        match rows.iter_mut().find(|p| p.id == proposal.id && p.status == from) {
            Some(row) => {
                row.status = proposal.status;
                row.outcome = proposal.outcome.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn recent(&self, limit: u32) -> Result<Vec<Proposal>, PolicyError> {
        Ok(self.rows.lock().unwrap().iter().rev().take(limit as usize).cloned().collect())
    }
}
