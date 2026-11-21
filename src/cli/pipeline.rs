//! Composition root for one review: config → git → Jira → checks → Claude → verification → history.
//! Shared by `qgate review` and the commit watcher.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use chrono::Utc;

use super::agents::{apply_agent_policy, build_agents, claude_config};
use super::caching::{ReviewCache, cache_tags, has_no_cache_trailer, wrap_test_agent};
use super::redact::redact_report;
use super::snapshot::{GitScenarioSnapshots, commit_messages, export_commit};
use super::tickets::{TicketLoad, load_ticket_context};
use super::tool_checks::{
    ToolChecks, ToolOutcome, boundaries_from_rulebook, discover_toolchains, fold_into_report, quality_languages,
};
use crate::conventions::application::{RulebookStore, repo_key};
use crate::conventions::infrastructure::SqliteRulebookStore;
use crate::handoff::domain::{BlockingRules, HandoffInput, RunMeta as HandoffMeta};
use crate::handoff::{ExportReview, FsReviewFileSink, native_default_dir};
use crate::history::application::{RecordRun, ReviewRun, RunRepository, RunStatus, Trigger};
use crate::history::infrastructure::SqliteRunRepository;
use crate::impact::application::BaseRunStore;
use crate::quality::{ImportBoundary, Language};
use crate::review::application::ports::ChangeSource;
use crate::review::application::{
    AgentRun, CachingCodeReviewer, GatePolicy, Requirements, ReviewChanges, ReviewProgress, ReviewReport,
    ReviewSettings, TicketContext,
};
use crate::review::domain::change_set::ChangeSet;
use crate::review::infrastructure::anthropic::{AnthropicReviewer, AnthropicSettings, prompt_fingerprint};
use crate::review::infrastructure::check_runner::ProcessCheckRunner;
use crate::review::infrastructure::guidelines::load_guidelines;
use crate::review::infrastructure::local_git::{DiffTarget, LocalGit};
use crate::sandbox::application::ExecBackend;
use crate::sandbox::infrastructure::{backend_from_env, work_dir};
use crate::settings::application::{
    Config, CredentialName, CredentialStore, EnvLookup, LoadEffectiveSettings, Secret, ServerConfig, ServerConfigStore,
    SettingsDeps, SharedServerConfig,
};
use crate::settings::infrastructure::{
    AppPaths, CONFIG_FILE_NAME, SqliteServerConfigStore, TomlSeedSource, default_store, load_config, parse_config,
    process_env, resolve_credential,
};
use crate::tickets::application::{LoadOptions, TicketCache};
use crate::tickets::infrastructure::SqliteTicketCache;

pub(super) const HISTORY_DB_FILE: &str = "qgate.db";
const MAX_GUIDELINE_BYTES: usize = 20 * 1024;
const SHORT_SHA_LEN: usize = 7;
const DEFAULT_DASHBOARD_PORT: u16 = 8080;
const MISSING_API_KEY: &str = "no Anthropic API key: set ANTHROPIC_API_KEY (or ANTHROPIC_API_KEY_FILE), set it on the Settings page, or run `qgate auth set anthropic`";

/// Process-wide settings and services, loaded once.
pub struct AppContext {
    pub paths: AppPaths,
    /// Live server config: the settings page replaces it while the service runs.
    pub server: SharedServerConfig,
    pub store: Arc<dyn CredentialStore>,
    /// Versioned server settings in SQLite (the settings page writes here).
    pub settings_store: Arc<dyn ServerConfigStore>,
    pub env: EnvLookup,
}

impl AppContext {
    /// Effective settings: env > latest SQLite version > qgate.toml (seeds the database once) > defaults.
    pub fn load() -> Result<Self> {
        let paths = AppPaths::discover()?;
        let env = process_env();
        let db = paths.data_dir.join(HISTORY_DB_FILE);
        let settings_store: Arc<dyn ServerConfigStore> = Arc::new(
            SqliteServerConfigStore::open(&db).with_context(|| format!("cannot open settings in {}", db.display()))?,
        );
        let seed = Arc::new(TomlSeedSource::from_paths(&paths));
        let server = LoadEffectiveSettings::new(settings_store.clone(), seed, env.clone()).execute()?;
        let store: Arc<dyn CredentialStore> = Arc::from(default_store(&paths));
        Ok(Self { paths, server: Arc::new(RwLock::new(server)), store, settings_store, env })
    }

    /// Dependencies of the settings page; the current config is taken as the boot snapshot.
    pub fn settings_deps(&self) -> Arc<SettingsDeps> {
        Arc::new(SettingsDeps::new(
            self.settings_store.clone(),
            self.store.clone(),
            self.server.clone(),
            self.env.clone(),
        ))
    }

    /// A snapshot of the server config in effect right now.
    pub fn server(&self) -> ServerConfig {
        match self.server.read() {
            Ok(config) => config.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Where untrusted commands run: the isolated runner when `QGATE_RUNNER_SOCKET` is set, else here.
    pub fn exec_backend(&self) -> Arc<dyn ExecBackend> {
        backend_from_env(|name| (self.env)(name))
    }

    /// Parent of the commit snapshots (`QGATE_WORK_DIR`, default `<data_dir>/work`); shared with the runner.
    pub fn work_dir(&self) -> PathBuf {
        work_dir(|name| (self.env)(name), &self.paths.data_dir)
    }

    pub fn open_history(&self) -> Result<Arc<dyn RunRepository>> {
        let path = self.paths.data_dir.join(HISTORY_DB_FILE);
        let repository = SqliteRunRepository::open(&path)
            .with_context(|| format!("cannot open history database {}", path.display()))?;
        Ok(Arc::new(repository))
    }
}

pub struct ReviewRequest {
    pub repo_root: PathBuf,
    /// For `DiffTarget::Commit` / `Range`, full commit shas.
    pub target: DiffTarget,
    pub trigger: Trigger,
    pub explicit_ticket: Option<String>,
    /// Ticket is in progress: acceptance criteria are shown as progress but do not block the gate.
    /// The pipeline also sets this when the range's newest commit subject starts with `[WIP]`.
    pub ticket_in_progress: bool,
}

/// Whether `message`'s subject (first non-blank line) marks work in progress: it starts with
/// `[WIP]`, case-insensitively, after any leading whitespace.
fn subject_marks_in_progress(message: &str) -> bool {
    message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|subject| subject.to_ascii_lowercase().starts_with("[wip]"))
}

pub struct ReviewOutcome {
    pub report: ReviewReport,
    pub config: Config,
    /// Why acceptance criteria were not checked, when that is the case.
    pub ticket_note: Option<String>,
    pub run_id: Option<i64>,
}

/// What the tool checks need from the review that is already running.
struct ToolPrep<'a> {
    request: &'a ReviewRequest,
    server: &'a ServerConfig,
    db: &'a Path,
    exec: Arc<dyn ExecBackend>,
    work_dir: &'a Path,
    head: &'a Path,
    base_runs: Option<Arc<dyn BaseRunStore>>,
}

/// Head sha, base sha (the range base, or the commit's parent; `None` for a root commit) and the changes.
async fn load_change(
    root: &Path,
    target: &DiffTarget,
) -> Result<(String, Option<String>, ChangeSet), crate::review::application::ports::SourceError> {
    let git = LocalGit::new(root.to_path_buf(), target.clone())?;
    let (head, base) = match target {
        DiffTarget::Range { base, head } => (head.clone(), Some(base.clone())),
        DiffTarget::Commit(sha) => (sha.clone(), git.parent_of(sha).await?),
        DiffTarget::WorkingTree | DiffTarget::Base(_) => unreachable!("tool checks need a commit target"),
    };
    Ok((head, base, git.load().await?))
}

/// Git facts about the reviewed change, used for ticket detection and history.
struct RunMeta {
    repo_name: String,
    commit: Option<String>,
    branch: Option<String>,
    /// Messages of every reviewed commit (newest first), for ticket detection.
    commit_messages: Vec<String>,
}

pub struct ReviewPipeline {
    context: Arc<AppContext>,
    history: Option<Arc<dyn RunRepository>>,
}

impl ReviewPipeline {
    pub fn new(context: Arc<AppContext>, history: Option<Arc<dyn RunRepository>>) -> Self {
        Self { context, history }
    }

    /// Reviews and records the run (also when the review fails, so errors show on the dashboard).
    pub async fn run(&self, request: &ReviewRequest, progress: &dyn ReviewProgress) -> Result<ReviewOutcome> {
        let meta = collect_meta(request).await;
        let result = self.review(request, &meta, progress).await;
        let run_id = self.record(request, &meta, &result).await;
        if let Ok(outcome) = &result {
            self.export(request, &meta, outcome, run_id);
        }
        result.map(|outcome| ReviewOutcome { run_id, ..outcome })
    }

    /// Writes the Markdown review file for the developer's coding agent. Never fails the review.
    fn export(&self, request: &ReviewRequest, meta: &RunMeta, outcome: &ReviewOutcome, run_id: Option<i64>) {
        let server = self.context.server();
        let Some(commit) = meta.commit.clone() else { return };
        let sink = match FsReviewFileSink::new(&server.output.dir)
            .or_else(|_| FsReviewFileSink::new(native_default_dir(&request.repo_root)))
        {
            Ok(sink) => sink,
            Err(e) => {
                eprintln!("qgate: warning: cannot write review files: {e}");
                return;
            }
        };
        let export = match ExportReview::new(Arc::new(sink), &server.output) {
            Ok(export) => export,
            Err(e) => {
                eprintln!("qgate: warning: invalid output.path_template: {e}");
                return;
            }
        };
        let policy = apply_agent_policy(outcome.config.gate_policy(), &server);
        let port = server.dashboard.bind_addr().map(|a| a.port()).unwrap_or(DEFAULT_DASHBOARD_PORT);
        let input = HandoffInput {
            meta: HandoffMeta {
                repo: meta.repo_name.clone(),
                branch: meta.branch.clone(),
                commit,
                ticket_keys: outcome.report.ticket.as_ref().map(|t| t.keys.clone()).unwrap_or_default(),
                reviewed_at: Utc::now(),
                run_id,
                dashboard_url: Some(format!("http://localhost:{port}")),
            },
            report: &outcome.report,
            rules: BlockingRules::from(&policy),
        };
        let result = export.execute(&input);
        for error in &result.errors {
            eprintln!("qgate: warning: review file: {error}");
        }
    }

    async fn review(
        &self,
        request: &ReviewRequest,
        meta: &RunMeta,
        progress: &dyn ReviewProgress,
    ) -> Result<ReviewOutcome> {
        let root = &request.repo_root;
        let config = load_review_config(root, &request.target).await?;
        let api_key = resolve_credential(CredentialName::AnthropicApiKey, self.context.store.as_ref())?
            .context(MISSING_API_KEY)?;
        let server = self.context.server();
        let db = self.context.paths.data_dir.join(HISTORY_DB_FILE);
        let (exec, work_dir) = (self.context.exec_backend(), self.context.work_dir());
        let snapshots = Arc::new(GitScenarioSnapshots::new(work_dir.clone()));
        let cache = ReviewCache::open(&db, has_no_cache_trailer(&meta.commit_messages));
        let ids = Arc::new(LocalGit::new(root.clone(), request.target.clone())?);
        let mut agents = build_agents(&server, &api_key, &db, self.context.store.as_ref(), exec.clone(), snapshots)?;
        if let Some(agents) = agents.as_mut() {
            let agent_tags = cache_tags(
                &server.agents.model,
                &claude_config(&server).effort,
                crate::testing::application::prompt_fingerprint(),
            );
            wrap_test_agent(&mut agents.suite, &cache, ids.clone(), agent_tags);
        }
        let (requirements, ticket, ticket_notes) = self.tickets(&config, &server, request, meta, &db).await;
        let ticket_note = (!ticket_notes.is_empty()).then(|| ticket_notes.join("; "));
        let guidelines =
            load_guidelines(root, &config.review.guideline_files, &config.review.guidelines, MAX_GUIDELINE_BYTES).await;
        let checks = if server.watch.run_repo_checks { config.check_specs() } else { Vec::new() };

        let snapshot = match &request.target {
            DiffTarget::Commit(sha) | DiffTarget::Range { head: sha, .. } => {
                Some(export_commit(root, sha, &work_dir).await?)
            }
            _ => None,
        };
        let checks_root = snapshot.as_ref().map_or_else(|| root.clone(), |s| s.path().to_path_buf());

        let ai_settings = anthropic_settings(&config, &server.llm.base_url);
        let reviewer_tags = cache_tags(&ai_settings.model, &ai_settings.effort, prompt_fingerprint());
        let reviewer = CachingCodeReviewer::new(
            AnthropicReviewer::new(api_key.expose().to_string(), ai_settings)?,
            cache.cache.clone(),
            reviewer_tags,
            cache.no_cache,
        );
        let in_progress = request.ticket_in_progress || ticket_in_progress_from_range(&request.target, meta);
        let policy = apply_agent_policy(config.gate_policy(), &server);
        let settings = ReviewSettings {
            ignore_patterns: config.ignore_patterns(),
            max_file_bytes: config.max_file_bytes(),
            concurrency: config.llm.concurrency,
            guidelines,
            gate: policy.clone(),
            checks,
            block_acceptance: !in_progress,
            ..ReviewSettings::default()
        };
        let use_case = ReviewChanges::new(
            ids.as_ref().clone(),
            reviewer,
            ProcessCheckRunner::new(checks_root, exec.clone()),
            settings,
        )?;
        let commit = meta.commit.clone().unwrap_or_default();
        let agent_run = agents.as_ref().map(|a| AgentRun {
            suite: &a.suite,
            ticket: ticket.as_ref(),
            commit: &commit,
            repo_root: root,
            snapshot_root: snapshot.as_ref().map(|s| s.path()),
        });
        let mut report = use_case.execute_with_agents(requirements, ticket.clone(), agent_run, progress).await?;
        if let (Some(agents), Some(agent_report)) = (&agents, report.agents.as_mut()) {
            let spent = agents.budget.spent_usd();
            agent_report.cost_usd = Some(spent);
            agent_report.notes.extend(agents.notes.iter().cloned());
            report.estimated_cost_usd = Some(report.estimated_cost_usd.unwrap_or(0.0) + spent);
        }
        if let Some(snapshot) = snapshot.as_ref() {
            let prep = ToolPrep {
                request,
                server: &server,
                db: &db,
                exec,
                work_dir: &work_dir,
                head: snapshot.path(),
                base_runs: cache.base_runs(),
            };
            let outcome = self.tool_outcome(prep).await;
            let acceptance =
                GatePolicy { fail_on_unmet_criteria: policy.fail_on_unmet_criteria && !in_progress, ..policy };
            fold_into_report(&mut report, outcome, &acceptance);
        }
        self.redact(&mut report, &api_key);
        Ok(ReviewOutcome { report, config, ticket_note, run_id: None })
    }

    /// Quality tools and regression detection on the reviewed commit. Never fails: problems become notes.
    async fn tool_outcome(&self, prep: ToolPrep<'_>) -> ToolOutcome {
        let root = &prep.request.repo_root;
        let (head_sha, base_sha, changes) = match load_change(root, &prep.request.target).await {
            Ok(loaded) => loaded,
            Err(e) => {
                return ToolOutcome {
                    findings: Vec::new(),
                    notes: vec![format!("quality/regression checks skipped: {e}")],
                };
            }
        };
        let languages = quality_languages(&prep.server.agents.tests.languages);
        let mut notes = Vec::new();
        let regression_on = prep.server.regression.enabled;
        let base_snapshot = match (&base_sha, regression_on && languages.contains(&Language::Go)) {
            (Some(base), true) => export_commit(root, base, prep.work_dir)
                .await
                .map_err(|e| notes.push(format!("regression check skipped: cannot export the base commit: {e:#}")))
                .ok(),
            _ => None,
        };
        let checks = ToolChecks {
            exec: prep.exec,
            head_snapshot: prep.head,
            base_snapshot: base_snapshot.as_ref().map(|s| s.path()),
            base_sha,
            head_sha,
            changes,
            languages,
            boundaries: self.boundaries(root, prep.db, prep.server),
            toolchains: discover_toolchains(),
            quality_enabled: prep.server.quality.enabled,
            regression_enabled: regression_on && (base_snapshot.is_some() || notes.is_empty()),
            base_runs: prep.base_runs,
        };
        let mut outcome = checks.run().await;
        notes.extend(outcome.notes);
        outcome.notes = notes;
        outcome
    }

    /// Import boundaries from the repository's Rulebook; none (depguard off) when there is no usable rulebook.
    fn boundaries(&self, root: &Path, db: &Path, server: &ServerConfig) -> Vec<ImportBoundary> {
        let rulebook = SqliteRulebookStore::open(db).and_then(|store| store.load(&repo_key(root)));
        match rulebook {
            Ok(Some(rulebook)) => boundaries_from_rulebook(&rulebook, server.agents.architecture.use_inferred_rules),
            Ok(None) => Vec::new(),
            Err(e) => {
                eprintln!("qgate: warning: cannot read the rulebook for architecture boundaries: {e}");
                Vec::new()
            }
        }
    }

    /// Check output and quoted code are repository-controlled and may echo credentials.
    fn redact(&self, report: &mut ReviewReport, api_key: &Secret) {
        let stored: Vec<Secret> = CredentialName::ALL
            .into_iter()
            .filter_map(|name| resolve_credential(name, self.context.store.as_ref()).ok().flatten())
            .collect();
        let mut secrets = vec![api_key.expose()];
        secrets.extend(stored.iter().map(Secret::expose));
        let webhook = self.context.server().notify.webhook_url;
        if !webhook.is_empty() {
            secrets.push(&webhook);
        }
        redact_report(report, &secrets);
    }

    /// Jira problems never fail the review: the run continues without acceptance criteria and says why.
    async fn tickets(
        &self,
        config: &Config,
        server: &ServerConfig,
        request: &ReviewRequest,
        meta: &RunMeta,
        db: &Path,
    ) -> (Option<Requirements>, Option<TicketContext>, Vec<String>) {
        let jira = config.jira(&server.jira);
        let token = match resolve_credential(CredentialName::JiraApiToken, self.context.store.as_ref()) {
            Ok(token) => token,
            Err(e) => return (None, None, vec![format!("Jira token: {e}")]),
        };
        let analyst = &server.agents.ticket_analyst;
        let options = LoadOptions {
            include_parent_story: analyst.include_parent_story,
            include_description: analyst.use_description,
            cache_ttl: chrono::Duration::minutes(i64::from(analyst.cache_ttl_minutes)),
        };
        let cache = SqliteTicketCache::open(db).ok().map(|c| Arc::new(c) as Arc<dyn TicketCache>);
        let load = load_ticket_context(
            &jira,
            token.as_ref(),
            request.explicit_ticket.as_deref(),
            meta.branch.as_deref(),
            &meta.commit_messages,
            options,
            cache,
        )
        .await;
        match load {
            Ok(TicketLoad { loaded: Some(loaded), mut notes }) => {
                if loaded.requirements.criteria.is_empty() {
                    notes.push(format!("{} has no acceptance criteria to check", loaded.requirements.key));
                    return (None, Some(loaded.context), notes);
                }
                (Some(loaded.requirements), Some(loaded.context), notes)
            }
            Ok(TicketLoad { loaded: None, notes }) => (None, None, notes),
            Err(e) => (None, None, vec![format!("Jira: {e}")]),
        }
    }

    async fn record(&self, request: &ReviewRequest, meta: &RunMeta, result: &Result<ReviewOutcome>) -> Option<i64> {
        let history = self.history.clone()?;
        let run = build_run(request, meta, result);
        let recorded = tokio::task::spawn_blocking(move || RecordRun::new(history).execute(&run))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()));
        recorded.map_err(|e| eprintln!("qgate: warning: could not save the run to history: {e}")).ok()
    }
}

fn build_run(request: &ReviewRequest, meta: &RunMeta, result: &Result<ReviewOutcome>) -> ReviewRun {
    let (status, error, report) = match result {
        Ok(outcome) => (RunStatus::of_report(&outcome.report), None, Some(outcome.report.clone())),
        Err(e) => (RunStatus::Error, Some(format!("{e:#}")), None),
    };
    let ticket_key = report
        .as_ref()
        .and_then(|r| r.requirements.as_ref())
        .map(|r| r.requirements.key.clone())
        .or_else(|| request.explicit_ticket.clone());
    ReviewRun {
        id: 0,
        repo_path: request.repo_root.display().to_string(),
        repo_name: meta.repo_name.clone(),
        commit: meta.commit.clone(),
        branch: meta.branch.clone(),
        ticket_key,
        trigger: request.trigger,
        created_at: Utc::now(),
        status,
        error,
        report,
    }
}

/// The repository config that governs a review. For commits it is read from the PARENT (or base)
/// revision, so a commit cannot weaken its own gate, drop its checks or disable its own review.
/// Only an interactive working-tree review uses the working-tree file.
pub async fn load_review_config(root: &Path, target: &DiffTarget) -> Result<Config> {
    let git = LocalGit::new(root.to_path_buf(), DiffTarget::WorkingTree)?;
    let governing_rev = match target {
        DiffTarget::WorkingTree => return Ok(load_config(root)?),
        DiffTarget::Base(base) | DiffTarget::Range { base, .. } => Some(base.clone()),
        DiffTarget::Commit(sha) => git.parent_of(sha).await?,
    };
    let Some(rev) = governing_rev else {
        return Ok(Config::default()); // root commit: nothing older to trust
    };
    match git.read_file_at(&rev, CONFIG_FILE_NAME).await? {
        Some(text) => Ok(parse_config(&text, &format!("{}:{CONFIG_FILE_NAME}", short(&rev)))?),
        None => Ok(Config::default()),
    }
}

fn short(rev: &str) -> &str {
    rev.get(..SHORT_SHA_LEN).unwrap_or(rev)
}

/// Best effort: missing facts only weaken ticket detection; the run is still reviewed and recorded.
async fn collect_meta(request: &ReviewRequest) -> RunMeta {
    let repo_name = repo_name(&request.repo_root);
    let Ok(git) = LocalGit::new(request.repo_root.clone(), DiffTarget::WorkingTree) else {
        return RunMeta { repo_name, commit: None, branch: None, commit_messages: Vec::new() };
    };
    let commit = match &request.target {
        DiffTarget::Commit(sha) | DiffTarget::Range { head: sha, .. } => Some(sha.clone()),
        _ => git.head_commit().await.ok().flatten(),
    };
    let commit_messages = match &request.target {
        DiffTarget::Range { base, head } => commit_messages(&request.repo_root, Some(base), head).await,
        DiffTarget::Base(base) => commit_messages(&request.repo_root, Some(base), "HEAD").await,
        DiffTarget::Commit(sha) => commit_messages(&request.repo_root, None, sha).await,
        DiffTarget::WorkingTree => commit_messages(&request.repo_root, None, "HEAD").await,
    }
    .unwrap_or_default();
    RunMeta { repo_name, commit, branch: git.current_branch().await.ok().flatten(), commit_messages }
}

/// Detects a `[WIP]` tip for a commit-range review (the push flow): the newest reviewed commit is
/// `meta.commit_messages[0]`. Working-tree and base reviews rely only on the explicit request flag.
fn ticket_in_progress_from_range(target: &DiffTarget, meta: &RunMeta) -> bool {
    matches!(target, DiffTarget::Range { .. } | DiffTarget::Commit(_))
        && meta.commit_messages.first().is_some_and(|m| subject_marks_in_progress(m))
}

fn repo_name(root: &Path) -> String {
    root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Endpoint from the server config only: a repository must never redirect the API key elsewhere.
fn anthropic_settings(config: &Config, base_url: &str) -> AnthropicSettings {
    AnthropicSettings {
        base_url: base_url.to_string(),
        model: config.llm.model.clone(),
        effort: config.llm.effort.as_str().to_string(),
        max_tokens: config.llm.max_tokens,
        use_fallbacks: config.llm.fallbacks,
        ..AnthropicSettings::default()
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod pipeline_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::application::ReviewError;

    fn request() -> ReviewRequest {
        ReviewRequest {
            repo_root: PathBuf::from("/repos/shop-api"),
            target: DiffTarget::Commit("abc1234".into()),
            trigger: Trigger::Commit,
            explicit_ticket: Some("SHOP-7".into()),
            ticket_in_progress: false,
        }
    }

    fn meta() -> RunMeta {
        RunMeta {
            repo_name: "shop-api".into(),
            commit: Some("abc1234".into()),
            branch: Some("feature/SHOP-7".into()),
            commit_messages: Vec::new(),
        }
    }

    #[test]
    fn failed_review_is_recorded_as_error_with_message() {
        let result: Result<ReviewOutcome> = Err(anyhow::Error::new(ReviewError::InvalidIgnorePattern {
            pattern: "a[".into(),
            reason: "unclosed".into(),
        }));
        let run = build_run(&request(), &meta(), &result);
        assert_eq!(run.status, RunStatus::Error);
        assert!(run.error.unwrap().contains("a["));
        assert!(run.report.is_none());
        assert_eq!(run.ticket_key.as_deref(), Some("SHOP-7"));
        assert_eq!(run.repo_name, "shop-api");
    }

    #[test]
    fn anthropic_settings_follow_repo_config() {
        let mut config = Config::default();
        config.llm.model = "claude-sonnet-5-5".into();
        config.llm.fallbacks = false;
        let settings = anthropic_settings(&config, "https://api.anthropic.com");
        assert_eq!(settings.model, "claude-sonnet-5-5");
        assert!(!settings.use_fallbacks);
        assert_eq!(settings.effort, config.llm.effort.as_str());
        assert_eq!(settings.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn repo_name_is_the_last_path_component() {
        assert_eq!(repo_name(Path::new("/repos/shop-api")), "shop-api");
    }

    #[test]
    fn wip_subject_is_detected_case_insensitively_with_leading_whitespace() {
        assert!(subject_marks_in_progress("[WIP] add cart"));
        assert!(subject_marks_in_progress("  [wip] add cart"));
        assert!(subject_marks_in_progress("\n\n[Wip] after blank lines"));
        assert!(subject_marks_in_progress("[WIP]"));
        assert!(!subject_marks_in_progress("feat: [WIP] not at the start"));
        assert!(!subject_marks_in_progress("add cart\n[WIP] in the body"));
        assert!(!subject_marks_in_progress("WIP: no brackets"));
        assert!(!subject_marks_in_progress(""));
    }

    #[test]
    fn in_progress_from_range_only_for_commit_and_range_targets() {
        let meta = |subject: &str| RunMeta {
            repo_name: "r".into(),
            commit: None,
            branch: None,
            commit_messages: vec![subject.into()],
        };
        let range = DiffTarget::Range { base: "b".into(), head: "h".into() };
        assert!(ticket_in_progress_from_range(&range, &meta("[WIP] x")));
        assert!(ticket_in_progress_from_range(&DiffTarget::Commit("abc1234".into()), &meta("[WIP] x")));
        assert!(!ticket_in_progress_from_range(&range, &meta("done")));
        // Working-tree reviews ignore the message; only the explicit flag applies there.
        assert!(!ticket_in_progress_from_range(&DiffTarget::WorkingTree, &meta("[WIP] x")));
        assert!(!ticket_in_progress_from_range(&DiffTarget::Base("main".into()), &meta("[WIP] x")));
    }

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn load_change_resolves_base_head_and_changes_for_commit_and_range() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-q", "-m", "one"]);
        let first = git_in(dir.path(), &["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("b.go"), "package b\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-q", "-m", "two"]);
        let second = git_in(dir.path(), &["rev-parse", "HEAD"]);

        let (head, base, changes) = load_change(dir.path(), &DiffTarget::Commit(second.clone())).await.unwrap();
        assert_eq!((head.as_str(), base.as_deref()), (second.as_str(), Some(first.as_str())));
        assert_eq!(changes.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["b.go"]);

        let (_, root_base, _) = load_change(dir.path(), &DiffTarget::Commit(first.clone())).await.unwrap();
        assert_eq!(root_base, None, "a root commit has no base");

        let range = DiffTarget::Range { base: first.clone(), head: second.clone() };
        let (head, base, changes) = load_change(dir.path(), &range).await.unwrap();
        assert_eq!((head, base), (second, Some(first)));
        assert_eq!(changes.files.len(), 1);
    }
}
