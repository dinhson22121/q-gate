use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;

use super::agents::{apply_agent_policy, build_agents, effort_name};
use super::caching::{ReviewCache, cache_tags, wrap_test_agent};
use super::tickets::{TicketLoad, load_ticket_context};
use crate::cache::application::NoCacheTrailer;
use crate::conventions::application::{RulebookStore, repo_key};
use crate::conventions::infrastructure::SqliteRulebookStore;
use crate::handoff::domain::{BlockingRules, HandoffInput, RunMeta as HandoffMeta};
use crate::handoff::{ExportReview, FsReviewFileSink, native_default_dir};
use crate::history::application::{RecordRun, ReviewRun, RunRepository, RunStatus, Trigger};
use crate::history::infrastructure::SqliteRunRepository;
use crate::llm::{CliClient, CliConfig, LlmError, StructuredModel};
use crate::review::application::redaction::redact_report;
use crate::review::application::work_in_progress::subject_marks_in_progress;
use crate::review::application::{
    AgentPlan, AgentRun, CachingCodeReviewer, Requirements, ReviewChanges, ReviewPreflight, ReviewProgress,
    ReviewReport, ReviewSettings, RunSupplementaryChecks, SupplementarySettings, TicketContext,
};
use crate::review::infrastructure::check_runner::ProcessCheckRunner;
use crate::review::infrastructure::claude::{ClaudeReviewer, prompt_fingerprint};
use crate::review::infrastructure::claude_preflight::ClaudePreflight;
use crate::review::infrastructure::commit_snapshots::GitCommitSnapshots;
use crate::review::infrastructure::guidelines::load_guidelines;
use crate::review::infrastructure::local_git::{DiffTarget, LocalGit};
use crate::review::infrastructure::snapshot::{GitScenarioSnapshots, commit_messages, export_commit};
use crate::review::infrastructure::tool_checks::{
    GitCommitChanges, QualityTools, RegressionTools, languages_from_settings, rulebook_boundaries,
};
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
const ENV_CLAUDE_OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";

pub struct AppContext {
    pub paths: AppPaths,
    pub server: SharedServerConfig,
    pub store: Arc<dyn CredentialStore>,
    pub settings_store: Arc<dyn ServerConfigStore>,
    pub env: EnvLookup,
    pub model_override: Option<Arc<dyn StructuredModel>>,
}

impl AppContext {
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
        Ok(Self { paths, server: Arc::new(RwLock::new(server)), store, settings_store, env, model_override: None })
    }

    pub fn settings_deps(&self) -> Arc<SettingsDeps> {
        Arc::new(SettingsDeps::new(
            self.settings_store.clone(),
            self.store.clone(),
            self.server.clone(),
            self.env.clone(),
        ))
    }

    pub fn server(&self) -> ServerConfig {
        match self.server.read() {
            Ok(config) => config.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn structured_model(&self, model: &str, effort: &str) -> Result<Arc<dyn StructuredModel>, LlmError> {
        if let Some(model) = &self.model_override {
            return Ok(model.clone());
        }
        Ok(Arc::new(CliClient::new(self.cli_config(model, effort))?))
    }

    pub fn cli_config(&self, model: &str, effort: &str) -> CliConfig {
        let llm = self.server().llm;
        CliConfig {
            claude_bin: PathBuf::from(llm.claude_bin),
            model: model.into(),
            effort: effort.into(),
            timeout: Duration::from_secs(llm.timeout_secs),
        }
    }

    pub fn exec_backend(&self) -> Arc<dyn ExecBackend> {
        backend_from_env(|name| (self.env)(name))
    }

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
    pub target: DiffTarget,
    pub trigger: Trigger,
    pub explicit_ticket: Option<String>,
    pub ticket_in_progress: bool,
}

pub struct ReviewOutcome {
    pub report: ReviewReport,
    pub config: Config,
    pub ticket_note: Option<String>,
    pub run_id: Option<i64>,
}

struct RunMeta {
    repo_name: String,
    commit: Option<String>,
    branch: Option<String>,
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

    pub async fn run(&self, request: &ReviewRequest, progress: &dyn ReviewProgress) -> Result<ReviewOutcome> {
        let meta = collect_meta(request).await;
        let result = self.review(request, &meta, progress).await;
        let run_id = self.record(request, &meta, &result).await;
        if let Ok(outcome) = &result {
            self.export(request, &meta, outcome, run_id);
        }
        result.map(|outcome| ReviewOutcome { run_id, ..outcome })
    }

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
        let server = self.context.server();
        let agent_model = self.context.structured_model(&server.agents.model, effort_name(&server))?;
        let preflight = ReviewPreflight::new(Arc::new(ClaudePreflight::new(
            self.context.structured_model(&config.llm.model, config.llm.effort.as_str())?,
        )));
        preflight.ensure_available().await?;
        let db = self.context.paths.data_dir.join(HISTORY_DB_FILE);
        let (exec, work_dir) = (self.context.exec_backend(), self.context.work_dir());
        let snapshots = Arc::new(GitScenarioSnapshots::new(work_dir.clone()));
        let cache = ReviewCache::open(&db, NoCacheTrailer::is_in(&meta.commit_messages));
        let ids = Arc::new(LocalGit::new(root.clone(), request.target.clone())?);
        let mut agents = build_agents(
            &server,
            agent_model,
            &self.context.cli_config(&server.agents.model, effort_name(&server)),
            &db,
            self.context.store.as_ref(),
            exec.clone(),
            snapshots,
        )?;
        if let Some(agents) = agents.as_mut() {
            let agent_tags = cache_tags(
                &server.agents.model,
                effort_name(&server),
                crate::testing::application::prompt_fingerprint(),
            );
            wrap_test_agent(&mut agents.suite, &cache, ids.clone(), agent_tags);
        }
        let (requirements, ticket, ticket_notes) = self.tickets(&config, &server, request, meta, &db).await;
        let ticket_note = (!ticket_notes.is_empty()).then(|| ticket_notes.join("; "));
        let guidelines =
            load_guidelines(root, &config.review.guideline_files, &config.review.guidelines, MAX_GUIDELINE_BYTES).await;
        let checks = if server.watch.run_repo_checks { config.check_specs() } else { Vec::new() };
        let plan = AgentPlan {
            agents_enabled: agents.is_some(),
            scenarios: server.agents.tests.max_scenarios as usize,
            tests_enabled: server.agents.tests.enabled,
            architecture_enabled: server.agents.architecture.enabled,
        };
        preflight.log_estimate(ids.as_ref(), plan, requirements.as_ref(), &guidelines).await;

        let snapshot = match &request.target {
            DiffTarget::Commit(sha) | DiffTarget::Range { head: sha, .. } => {
                Some(export_commit(root, sha, &work_dir).await?)
            }
            _ => None,
        };
        let checks_root = snapshot.as_ref().map_or_else(|| root.clone(), |s| s.path().to_path_buf());

        let (review_model, review_effort) = (&config.llm.model, config.llm.effort.as_str());
        let reviewer_tags = cache_tags(review_model, review_effort, prompt_fingerprint());
        let reviewer = CachingCodeReviewer::new(
            ClaudeReviewer::new(self.context.structured_model(review_model, review_effort)?),
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
        if let Some(agents) = &agents {
            preflight.settle_agent_spend(&mut report, agents.meter.cost_usd(), &agents.notes)?;
        }
        if let Some(snapshot) = snapshot.as_ref() {
            let supplementary = self.supplementary_checks(request, &server, &db, &work_dir, &exec, &cache)?;
            supplementary.execute(snapshot.path(), &mut report, policy, in_progress).await;
        }
        self.redact(&mut report);
        Ok(ReviewOutcome { report, config, ticket_note, run_id: None })
    }

    fn supplementary_checks(
        &self,
        request: &ReviewRequest,
        server: &ServerConfig,
        db: &Path,
        work_dir: &Path,
        exec: &Arc<dyn ExecBackend>,
        cache: &ReviewCache,
    ) -> Result<RunSupplementaryChecks> {
        let languages = languages_from_settings(&server.agents.tests.languages);
        let rulebook = SqliteRulebookStore::open(db).and_then(|store| store.load(&repo_key(&request.repo_root)));
        let boundaries = rulebook_boundaries(rulebook, server.agents.architecture.use_inferred_rules);
        let quality = QualityTools::new(exec.clone(), &languages, boundaries, QualityTools::discover_toolchains());
        Ok(RunSupplementaryChecks::new(
            Arc::new(GitCommitChanges::new(request.repo_root.clone(), request.target.clone())?),
            Arc::new(GitCommitSnapshots::new(request.repo_root.clone(), work_dir.to_path_buf())),
            Arc::new(quality),
            Arc::new(RegressionTools::new(exec.clone(), cache.base_runs())),
            SupplementarySettings {
                languages,
                quality_enabled: server.quality.enabled,
                regression_enabled: server.regression.enabled,
            },
        ))
    }

    fn redact(&self, report: &mut ReviewReport) {
        let stored: Vec<Secret> = CredentialName::ALL
            .into_iter()
            .filter_map(|name| resolve_credential(name, self.context.store.as_ref()).ok().flatten())
            .collect();
        let mut secrets: Vec<&str> = stored.iter().map(Secret::expose).collect();
        let webhook = self.context.server().notify.webhook_url;
        if !webhook.is_empty() {
            secrets.push(&webhook);
        }
        let claude_token = (self.context.env)(ENV_CLAUDE_OAUTH_TOKEN).unwrap_or_default();
        if !claude_token.is_empty() {
            secrets.push(&claude_token);
        }
        redact_report(report, &secrets);
    }

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

pub async fn load_review_config(root: &Path, target: &DiffTarget) -> Result<Config> {
    let git = LocalGit::new(root.to_path_buf(), DiffTarget::WorkingTree)?;
    let governing_rev = match target {
        DiffTarget::WorkingTree => return Ok(load_config(root)?),
        DiffTarget::Base(base) | DiffTarget::Range { base, .. } => Some(base.clone()),
        DiffTarget::Commit(sha) => git.parent_of(sha).await?,
    };
    let Some(rev) = governing_rev else {
        return Ok(Config::default());
    };
    match git.read_file_at(&rev, CONFIG_FILE_NAME).await? {
        Some(text) => Ok(parse_config(&text, &format!("{}:{CONFIG_FILE_NAME}", short(&rev)))?),
        None => Ok(Config::default()),
    }
}

fn short(rev: &str) -> &str {
    rev.get(..SHORT_SHA_LEN).unwrap_or(rev)
}

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

fn ticket_in_progress_from_range(target: &DiffTarget, meta: &RunMeta) -> bool {
    matches!(target, DiffTarget::Range { .. } | DiffTarget::Commit(_))
        && meta.commit_messages.first().is_some_and(|m| subject_marks_in_progress(m))
}

fn repo_name(root: &Path) -> String {
    root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned())
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
    fn repo_name_is_the_last_path_component() {
        assert_eq!(repo_name(Path::new("/repos/shop-api")), "shop-api");
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
        assert!(!ticket_in_progress_from_range(&DiffTarget::WorkingTree, &meta("[WIP] x")));
        assert!(!ticket_in_progress_from_range(&DiffTarget::Base("main".into()), &meta("[WIP] x")));
    }
}
