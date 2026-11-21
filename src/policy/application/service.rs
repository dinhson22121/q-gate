//! Policy use cases: propose (describe → structured plan), preview, dry-run on history, approve/reject,
//! re-preview, and the "prevent this" prefill. Nothing is applied before an explicit approval, and every
//! change goes through the settings and conventions use cases.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use super::ports::{
    GatePolicySource, PolicyError, PolicyModel, PolicyPrompt, ProposalStore, RepoGate, RuleWrite, RulebookGateway,
    RunHistory, SettingsGateway, SettingsSnapshot,
};
use crate::policy::domain::json::{get, set};
use crate::policy::domain::proposal::clip;
use crate::policy::domain::{
    ApplyOutcome, ApprovalError, ApprovalRequest, DryRunReport, ExistingRule, MAX_DESCRIPTION_CHARS, PreviewSection,
    Proposal, ProposalContent, ProposalContext, ProposalDraft, ProposalStatus, RejectedItem, RunFacts, render_preview,
    repo_patch, settable_paths, summarize, validate_draft, with_gate_value,
};
use crate::review::application::GatePolicy;

/// `changed_by` prefix of settings versions written on approval.
pub const CHANGED_BY_PREFIX: &str = "policy-assistant:p";
pub const DEFAULT_DRY_RUN_RUNS: u32 = 50;
pub const MAX_DRY_RUN_RUNS: u32 = 200;
const RECENT_PROPOSALS: u32 = 20;
const PREFILL_EVIDENCE_CHARS: usize = 1_500;

/// Everything the use cases need. Build once per process.
pub struct PolicyDeps {
    pub model: Arc<dyn PolicyModel>,
    pub settings: Arc<dyn SettingsGateway>,
    pub rulebooks: Arc<dyn RulebookGateway>,
    pub history: Arc<dyn RunHistory>,
    pub gates: Arc<dyn GatePolicySource>,
    pub store: Arc<dyn ProposalStore>,
    pub now: fn() -> DateTime<Utc>,
}

/// A proposal with its rendered preview and whether it can still be approved.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProposalView {
    pub proposal: Proposal,
    pub preview: Vec<PreviewSection>,
    pub repo_patch: Option<String>,
    pub expired: bool,
    /// The settings changed since the proposal was made: approve is refused until it is re-previewed.
    pub stale: bool,
    pub current_settings_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposalSummary {
    pub id: i64,
    pub status: ProposalStatus,
    pub created_at: DateTime<Utc>,
    pub repo: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prefill {
    pub description: String,
    pub repo: String,
}

async fn blocking<T, F>(work: F) -> Result<T, PolicyError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PolicyError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| PolicyError::Storage(format!("background worker failed: {err}")))?
}

fn approval_error(err: ApprovalError) -> PolicyError {
    match err {
        ApprovalError::NotDraft(_) | ApprovalError::Expired | ApprovalError::Stale { .. } => {
            PolicyError::Conflict(err.to_string())
        }
        ApprovalError::NeedsConfirmation(_) => PolicyError::NeedsConfirmation(err.to_string()),
        ApprovalError::NothingSelected | ApprovalError::UnknownItem(..) => PolicyError::Invalid(err.to_string()),
    }
}

/// `config` with every change applied.
fn patched(config: &Value, changes: impl IntoIterator<Item = (String, Value)>) -> Value {
    let mut next = config.clone();
    for (path, value) in changes {
        set(&mut next, &path, value);
    }
    next
}

pub struct PolicyService {
    deps: PolicyDeps,
    /// One approval at a time: the stale check and the writes happen under it.
    approve_lock: AsyncMutex<()>,
}

impl PolicyService {
    pub fn new(deps: PolicyDeps) -> Self {
        Self { deps, approve_lock: AsyncMutex::new(()) }
    }

    async fn snapshot(&self) -> Result<SettingsSnapshot, PolicyError> {
        let settings = self.deps.settings.clone();
        blocking(move || settings.snapshot()).await
    }

    async fn load(&self, id: i64) -> Result<Proposal, PolicyError> {
        let store = self.deps.store.clone();
        blocking(move || store.get(id))
            .await?
            .ok_or_else(|| PolicyError::NotFound(format!("proposal {id} does not exist")))
    }

    /// Repositories a proposal may target: those with a rulebook or a recorded run. Repository paths
    /// are never taken from the request as-is, so no arbitrary path is read.
    pub async fn repos(&self) -> Result<Vec<String>, PolicyError> {
        let history = self.deps.history.clone();
        let mut repos: BTreeSet<String> = blocking(move || history.repo_paths()).await?.into_iter().collect();
        repos.extend(self.deps.rulebooks.repos().await?);
        Ok(repos.into_iter().collect())
    }

    async fn known_repo(&self, repo: Option<String>) -> Result<Option<String>, PolicyError> {
        let Some(repo) = repo.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) else { return Ok(None) };
        if self.repos().await?.contains(&repo) {
            Ok(Some(repo))
        } else {
            Err(PolicyError::Invalid(format!("unknown repository {repo}")))
        }
    }

    /// Describe → structured proposal (stored as a draft). Applies nothing.
    pub async fn propose(&self, description: &str, repo: Option<String>) -> Result<ProposalView, PolicyError> {
        let description = description.trim();
        if description.is_empty() {
            return Err(PolicyError::Invalid("describe how your team reviews code first".into()));
        }
        if description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(PolicyError::Invalid(format!(
                "the description is longer than {MAX_DESCRIPTION_CHARS} characters"
            )));
        }
        let repo = self.known_repo(repo).await?;
        let (snapshot, rules, gate) = self.context_for(repo.as_deref()).await?;
        let prompt = PolicyPrompt {
            description: description.to_string(),
            settings: settable_paths(&snapshot.config)
                .into_iter()
                .filter_map(|path| get(&snapshot.config, &path).cloned().map(|value| (path, value)))
                .collect(),
            locked: snapshot.locked.clone(),
            repo: repo.clone(),
            rules: rules.clone(),
            repo_gate: gate.as_ref().map(|g| g.policy.clone()),
            existing_checks: gate.as_ref().map(|g| g.check_names.clone()).unwrap_or_default(),
        };
        let draft = self.deps.model.propose(&prompt).await?;
        self.store_new(description.to_string(), repo, draft, snapshot, &rules, gate).await
    }

    async fn context_for(
        &self,
        repo: Option<&str>,
    ) -> Result<(SettingsSnapshot, Vec<ExistingRule>, Option<RepoGate>), PolicyError> {
        let snapshot = self.snapshot().await?;
        let Some(repo) = repo else { return Ok((snapshot, Vec::new(), None)) };
        let rules = self.deps.rulebooks.rules(repo).await?;
        let (gates, path) = (self.deps.gates.clone(), repo.to_string());
        let gate = blocking(move || Ok(gates.repo_gate(&path))).await?;
        Ok((snapshot, rules, Some(gate)))
    }

    async fn store_new(
        &self,
        description: String,
        repo: Option<String>,
        draft: ProposalDraft,
        snapshot: SettingsSnapshot,
        rules: &[ExistingRule],
        gate: Option<RepoGate>,
    ) -> Result<ProposalView, PolicyError> {
        let (check_names, repo_gate) = gate.map(|g| (g.check_names, Some(g.policy))).unwrap_or_default();
        let context = ProposalContext {
            settings: &snapshot.config,
            locked: &snapshot.locked,
            restart_required: &snapshot.restart_required,
            repo: repo.as_deref(),
            rules,
            repo_gate: repo_gate.as_ref(),
            existing_checks: &check_names,
        };
        let mut content = validate_draft(&draft, &context);
        let settings = self.deps.settings.clone();
        let config = snapshot.config.clone();
        content = blocking(move || Ok(check_settings(settings.as_ref(), &config, content))).await?;
        let mut proposal = Proposal::new(description, repo, snapshot.version, draft, content, (self.deps.now)());
        let store = self.deps.store.clone();
        let stored = proposal.clone();
        proposal.id = blocking(move || store.insert(&stored)).await?;
        Ok(self.view(proposal, &snapshot))
    }

    fn view(&self, proposal: Proposal, snapshot: &SettingsSnapshot) -> ProposalView {
        let preview = render_preview(&proposal.content, proposal.repo.as_deref(), snapshot.webhook_configured);
        let patch = repo_patch(&proposal.content.gate, &proposal.content.checks);
        ProposalView {
            expired: proposal.is_expired((self.deps.now)()),
            stale: proposal.status == ProposalStatus::Draft && proposal.settings_version != snapshot.version,
            current_settings_version: snapshot.version,
            repo_patch: patch,
            preview,
            proposal,
        }
    }

    pub async fn get(&self, id: i64) -> Result<ProposalView, PolicyError> {
        let proposal = self.load(id).await?;
        let snapshot = self.snapshot().await?;
        Ok(self.view(proposal, &snapshot))
    }

    pub async fn recent(&self) -> Result<Vec<ProposalSummary>, PolicyError> {
        let store = self.deps.store.clone();
        let proposals = blocking(move || store.recent(RECENT_PROPOSALS)).await?;
        Ok(proposals
            .into_iter()
            .map(|p| ProposalSummary {
                id: p.id,
                status: p.status,
                created_at: p.created_at,
                repo: p.repo,
                summary: p.content.summary,
            })
            .collect())
    }

    /// Validates the stored draft again against the current settings and rulebook, as a new proposal (no
    /// model call). The old draft is marked rejected.
    pub async fn repreview(&self, id: i64) -> Result<ProposalView, PolicyError> {
        let old = self.load(id).await?;
        if old.status == ProposalStatus::Applied {
            return Err(PolicyError::Conflict("this proposal was already applied".into()));
        }
        let (snapshot, rules, gate) = self.context_for(old.repo.as_deref()).await?;
        let view = self
            .store_new(old.description.clone(), old.repo.clone(), old.draft.clone(), snapshot, &rules, gate)
            .await?;
        if old.status == ProposalStatus::Draft {
            let store = self.deps.store.clone();
            let rejected = Proposal { status: ProposalStatus::Rejected, ..old };
            blocking(move || store.transition(&rejected, ProposalStatus::Draft)).await?;
        }
        Ok(view)
    }

    pub async fn reject(&self, id: i64) -> Result<ProposalView, PolicyError> {
        let proposal = self.load(id).await?;
        let rejected = Proposal { status: ProposalStatus::Rejected, ..proposal };
        let (store, saved) = (self.deps.store.clone(), rejected.clone());
        if !blocking(move || store.transition(&saved, ProposalStatus::Draft)).await? {
            return Err(PolicyError::Conflict("only a draft can be rejected".into()));
        }
        let snapshot = self.snapshot().await?;
        Ok(self.view(rejected, &snapshot))
    }

    /// Replays the deterministic gate over the newest `limit` runs with the current and the proposed policy.
    pub async fn dry_run(&self, id: i64, limit: Option<u32>) -> Result<DryRunReport, PolicyError> {
        let proposal = self.load(id).await?;
        let limit = limit.unwrap_or(DEFAULT_DRY_RUN_RUNS).clamp(1, MAX_DRY_RUN_RUNS);
        let (history, gates, settings) =
            (self.deps.history.clone(), self.deps.gates.clone(), self.deps.settings.clone());
        blocking(move || {
            let current = settings.snapshot()?.config;
            let proposed = patched(&current, proposal.content.settings.iter().map(|c| (c.path.clone(), c.new.clone())));
            let (runs, skipped) = history.recent(limit)?;
            let repo_gates: HashMap<&str, GatePolicy> = runs
                .iter()
                .map(|r| r.repo_path.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|path| (path, gates.repo_gate(path).policy))
                .collect();
            let repo_policy = |run: &RunFacts| repo_gates.get(run.repo_path.as_str()).cloned().unwrap_or_default();
            let before = |run: &RunFacts| gates.with_server(repo_policy(run), &current);
            let after = |run: &RunFacts| {
                let mut policy = repo_policy(run);
                if proposal.repo.as_deref() == Some(run.repo_path.as_str()) {
                    for change in &proposal.content.gate {
                        policy = with_gate_value(&policy, change.field, &change.new).unwrap_or(policy);
                    }
                }
                gates.with_server(policy, &proposed)
            };
            Ok(summarize(&runs, skipped, !proposal.content.rules.is_empty(), before, after))
        })
        .await
    }

    /// Applies the selected items: settings as one new version, rules into the rulebook. Refused when the
    /// proposal is not a draft, expired, or the settings changed since it was made.
    pub async fn approve(&self, id: i64, request: ApprovalRequest) -> Result<ProposalView, PolicyError> {
        let _guard = self.approve_lock.lock().await;
        let mut proposal = self.load(id).await?;
        let snapshot = self.snapshot().await?;
        let plan = proposal.plan(&request, snapshot.version, (self.deps.now)()).map_err(approval_error)?;
        let mut outcome = ApplyOutcome::default();
        if !plan.settings.is_empty() {
            let changes: Vec<(String, Value)> = plan.settings.iter().map(|c| (c.path.clone(), c.new.clone())).collect();
            let (settings, expected) = (self.deps.settings.clone(), proposal.settings_version);
            let changed_by = format!("{CHANGED_BY_PREFIX}{id}");
            let applied = blocking(move || settings.apply(&changes, &changed_by, expected)).await?;
            outcome.settings_version = applied.version;
            outcome.settings_applied = applied.changed_fields;
            outcome.restart_required = applied.restart_required;
        }
        if !plan.rules.is_empty() {
            let repo = proposal.repo.clone().ok_or_else(|| PolicyError::Invalid("rules need a repository".into()))?;
            let writes = plan
                .rules
                .into_iter()
                .map(|(rule, status)| RuleWrite {
                    replaces: rule.replaces,
                    title: rule.title,
                    description: rule.description,
                    scope_globs: rule.scope_globs,
                    status,
                })
                .collect();
            match self.deps.rulebooks.write(&repo, writes).await {
                Ok(titles) => outcome.rules_written = titles,
                Err(err) if outcome.settings_version.is_some() => {
                    outcome.errors.push(format!("settings were applied but the rules were not: {err}"));
                }
                Err(err) => return Err(err),
            }
        }
        proposal.status = ProposalStatus::Applied;
        proposal.outcome = Some(outcome);
        let (store, saved) = (self.deps.store.clone(), proposal.clone());
        blocking(move || store.transition(&saved, ProposalStatus::Draft)).await?;
        let snapshot = self.snapshot().await?;
        Ok(self.view(proposal, &snapshot))
    }

    /// "Prevent this": a description pre-filled with a stored finding's context.
    pub async fn prefill(&self, run_id: i64, index: usize) -> Result<Prefill, PolicyError> {
        let history = self.deps.history.clone();
        let finding = blocking(move || history.finding(run_id, index))
            .await?
            .ok_or_else(|| PolicyError::NotFound(format!("run {run_id} has no finding {index}")))?;
        let mut text = format!(
            "Don't let this happen again. qgate found this in run {run_id}:\n\
             [{} / {}] {} at {}:{}\n",
            finding.severity, finding.category, finding.title, finding.file, finding.line
        );
        if !finding.explanation.trim().is_empty() {
            text.push_str(&format!("Why it matters: {}\n", clip(&finding.explanation, PREFILL_EVIDENCE_CHARS)));
        }
        if !finding.evidence.trim().is_empty() {
            text.push_str(&format!("Code:\n{}\n", clip(&finding.evidence, PREFILL_EVIDENCE_CHARS)));
        }
        text.push_str(
            "\nPropose a team rule for this repository (scoped to the files where it matters) and, if a linter or \
             test could catch it mechanically, a check.",
        );
        Ok(Prefill { description: text, repo: finding.repo_path })
    }
}

/// Runs the settings validation on each change alone and on all of them together; failures become rejected items.
fn check_settings(settings: &dyn SettingsGateway, config: &Value, mut content: ProposalContent) -> ProposalContent {
    let mut kept = Vec::new();
    for change in std::mem::take(&mut content.settings) {
        match settings.validate(&patched(config, [(change.path.clone(), change.new.clone())])) {
            Ok(()) => kept.push(change),
            Err(message) => content.rejected.push(RejectedItem::new("setting", change.path, message)),
        }
    }
    if kept.len() > 1
        && let Err(message) = settings.validate(&patched(config, kept.iter().map(|c| (c.path.clone(), c.new.clone()))))
    {
        for change in kept.drain(..) {
            content.rejected.push(RejectedItem::new(
                "setting",
                change.path,
                format!("conflicts with the other changes: {message}"),
            ));
        }
    }
    content.settings = kept;
    content
}
