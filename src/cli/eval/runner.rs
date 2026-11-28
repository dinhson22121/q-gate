use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::BoxFuture;

use crate::cli::agents::{apply_agent_policy, build_agents, effort_name};
use crate::cli::pipeline::{AppContext, HISTORY_DB_FILE};
use crate::eval::application::{CurrentProvenance, EvalError, RepoModel, RepoModels, ReviewJob, ReviewRunner};
use crate::history::application::RunProvenance;
use crate::review::application::{AgentRun, NoProgress, ReviewChanges, ReviewReport, ReviewSettings};
use crate::review::infrastructure::callers::CallgraphCallers;
use crate::review::infrastructure::check_runner::ProcessCheckRunner;
use crate::review::infrastructure::claude::{ClaudeAuditor, ClaudeReviewer};
use crate::review::infrastructure::local_git::{DiffTarget, LocalGit};
use crate::review::infrastructure::snapshot::{GitScenarioSnapshots, export_commit};
use crate::settings::application::Config;
use crate::settings::infrastructure::config_file::load_config;
use crate::watch::application::ports::RepoSource;

pub struct CliReviewRunner {
    context: Arc<AppContext>,
    config: Config,
}

impl CliReviewRunner {
    pub fn new(context: Arc<AppContext>, config: Config) -> Self {
        Self { context, config }
    }
}

impl ReviewRunner for CliReviewRunner {
    fn run<'a>(&'a self, job: &'a ReviewJob) -> BoxFuture<'a, Result<ReviewReport, EvalError>> {
        Box::pin(async move {
            review(&self.context, &self.config, job).await.map_err(|e| EvalError::Runner(format!("{e:#}")))
        })
    }
}

pub struct ContextProvenance {
    context: Arc<AppContext>,
}

impl ContextProvenance {
    pub fn new(context: Arc<AppContext>) -> Self {
        Self { context }
    }
}

impl CurrentProvenance for ContextProvenance {
    fn current<'a>(&'a self) -> BoxFuture<'a, RunProvenance> {
        Box::pin(async move { self.context.provenance(&Config::default().llm.model).await })
    }
}

pub struct RegisteredRepoModels<S> {
    repos: Arc<S>,
}

impl<S> RegisteredRepoModels<S> {
    pub fn new(repos: Arc<S>) -> Self {
        Self { repos }
    }
}

impl<S: RepoSource> RepoModels for RegisteredRepoModels<S> {
    fn list<'a>(&'a self) -> BoxFuture<'a, Vec<RepoModel>> {
        Box::pin(async move {
            let Ok(repos) = self.repos.list().await else { return Vec::new() };
            let mut models = Vec::new();
            for repo in repos {
                let root = repo.path.clone();
                let loaded = tokio::task::spawn_blocking(move || load_config(&root)).await;
                if let Ok(Ok(config)) = loaded {
                    models.push(RepoModel { repo: repo.name, model: config.llm.model });
                }
            }
            models
        })
    }
}

async fn review(context: &Arc<AppContext>, config: &Config, job: &ReviewJob) -> Result<ReviewReport> {
    let target = DiffTarget::Range { base: job.base.clone(), head: job.head.clone() };
    let server = context.server();
    let (exec, work_dir) = (context.exec_backend(), context.work_dir());
    let (model, effort) = (&config.llm.model, config.llm.effort.as_str());
    let settings = ReviewSettings {
        ignore_patterns: config.ignore_patterns(),
        max_file_bytes: config.max_file_bytes(),
        concurrency: config.llm.concurrency,
        gate: apply_agent_policy(config.gate_policy(), &server),
        ..ReviewSettings::default()
    };
    let source = LocalGit::new(job.repo.clone(), target)?;
    let reviewer = ClaudeReviewer::new(context.structured_model(model, effort)?);
    let mut use_case =
        ReviewChanges::new(source, reviewer, ProcessCheckRunner::new(job.repo.clone(), exec.clone()), settings)?
            .with_callers(Arc::new(CallgraphCallers::new(job.repo.clone())));
    if config.llm.audit_findings {
        use_case = use_case.with_auditor(Arc::new(ClaudeAuditor::new(context.structured_model(model, effort)?)));
    }
    if !job.with_tests {
        return Ok(use_case
            .execute_with_agents(job.requirements.clone(), job.ticket.clone(), None, &NoProgress)
            .await?);
    }

    let mut tests_only = server.clone();
    tests_only.agents.enabled = true;
    tests_only.agents.tests.enabled = true;
    tests_only.agents.ticket_analyst.enabled = true;
    tests_only.agents.ticket_analyst.use_description = false;
    tests_only.agents.architecture.enabled = false;
    tests_only.figma.enabled = false;
    let agent_model = context.structured_model(&server.agents.model, effort_name(&server))?;
    let agents = build_agents(
        &tests_only,
        agent_model,
        &context.cli_config(&server.agents.model, effort_name(&server)),
        &context.paths.data_dir.join(HISTORY_DB_FILE),
        context.store.as_ref(),
        exec,
        Arc::new(GitScenarioSnapshots::new(work_dir.clone())),
    )?
    .context("the test agent could not be built")?;
    let snapshot = export_commit(&job.repo, &job.head, &work_dir).await?;
    let run = AgentRun {
        suite: &agents.suite,
        ticket: job.ticket.as_ref(),
        commit: &job.head,
        repo_root: &job.repo,
        snapshot_root: Some(snapshot.path()),
    };
    let mut report =
        use_case.execute_with_agents(job.requirements.clone(), job.ticket.clone(), Some(run), &NoProgress).await?;
    let spent = agents.meter.cost_usd();
    if let Some(agent_report) = report.agents.as_mut() {
        agent_report.cost_usd = Some(spent);
        agent_report.notes.extend(agents.notes.iter().cloned());
    }
    report.estimated_cost_usd = Some(report.estimated_cost_usd.unwrap_or(0.0) + spent);
    Ok(report)
}
