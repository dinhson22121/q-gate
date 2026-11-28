mod render;
pub(super) mod runner;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::args::{DEFAULT_CASES_DIR, EvalAction, EvalArgs, EvalRunArgs, IMAGE_CASES_DIR};
use super::pipeline::{AppContext, HISTORY_DB_FILE};
use crate::eval::application::{
    CompareToBaseline, EvalOptions, EvalProgress, EvalStore, ImportRun, ListEvalRuns, RunEval,
};
use crate::eval::domain::scoring::CaseScore;
use crate::eval::infrastructure::{
    FsCaseRepository, GitChangeExporter, GitWorkspace, HistoryRunSource, SqliteEvalStore,
};
use crate::feedback::application::{CriterionVerifications, FeedbackService};
use crate::feedback::infrastructure::SqliteFeedbackStore;
use crate::review::application::ReviewPreflight;
use crate::review::infrastructure::claude_preflight::ClaudePreflight;
use crate::settings::application::Config;
use crate::settings::infrastructure::parse_config;
use render::{render_list, render_run};
use runner::CliReviewRunner;

pub const EXIT_EVAL_OK: u8 = 0;
pub const EXIT_EVAL_ERROR: u8 = 1;
pub const EXIT_EVAL_REGRESSION: u8 = 2;
const LISTED_RUNS: u32 = 30;

pub async fn eval(context: Arc<AppContext>, args: EvalArgs) -> ExitCode {
    let result = match args.action {
        None => run(context, args.run).await,
        Some(EvalAction::ImportRun { run_id, out, id }) => import_run(&context, run_id, &out, id).await,
        Some(EvalAction::List) => list(&context),
    };
    result.unwrap_or_else(|error| {
        eprintln!("qgate eval: error: {error:#}");
        ExitCode::from(EXIT_EVAL_ERROR)
    })
}

fn open_store(context: &AppContext) -> Result<Arc<dyn EvalStore>> {
    let path = context.paths.data_dir.join(HISTORY_DB_FILE);
    let store = SqliteEvalStore::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
    Ok(Arc::new(store))
}

struct StderrProgress;

impl EvalProgress for StderrProgress {
    fn scored(&self, score: &CaseScore, done: usize, total: usize) {
        let counts = score.counts();
        eprintln!(
            "qgate eval: [{done}/{total}] {} run {}: TP {} FN {} FP {} unlabeled {} · ${:.2} · {:.1}s",
            score.case_id,
            score.repetition + 1,
            counts.tp,
            counts.missed,
            counts.fp,
            counts.unlabeled,
            score.cost_usd,
            score.duration_ms as f64 / 1000.0
        );
        if let Some(tests) = &score.tests {
            eprintln!("qgate eval: [{done}/{total}] {} tests {}", score.case_id, tests.summary());
        }
    }
}

fn default_cases_dir(local_exists: bool) -> PathBuf {
    PathBuf::from(if local_exists { DEFAULT_CASES_DIR } else { IMAGE_CASES_DIR })
}

fn resolve_cases_dir(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| default_cases_dir(Path::new(DEFAULT_CASES_DIR).is_dir()))
}

fn resolve_config(args: &EvalRunArgs) -> Result<Config> {
    let mut config = match &args.config {
        Some(path) => {
            let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
            parse_config(&text, &path.display().to_string())?
        }
        None => Config::default(),
    };
    if let Some(model) = &args.model {
        config.llm.model.clone_from(model);
    }
    if let Some(effort) = args.effort {
        config.llm.effort = effort;
    }
    Ok(config)
}

async fn run(context: Arc<AppContext>, args: EvalRunArgs) -> Result<ExitCode> {
    let config = resolve_config(&args)?;
    let store = open_store(&context)?;
    let comparer = CompareToBaseline::new(store.clone());
    let baseline = args.baseline.map(|reference| comparer.resolve(reference)).transpose()?;
    ReviewPreflight::new(Arc::new(ClaudePreflight::new(
        context.structured_model(&config.llm.model, config.llm.effort.as_str())?,
    )))
    .ensure_available()
    .await?;
    let use_case = RunEval::new(
        Arc::new(FsCaseRepository::new(resolve_cases_dir(args.cases))),
        Arc::new(GitWorkspace),
        Arc::new(CliReviewRunner::new(context.clone(), config.clone())),
        store,
        Arc::new(chrono::Utc::now),
    );
    let options = EvalOptions { only: args.only, runs: args.runs, with_tests: args.with_tests };
    let provenance = context.provenance(&config.llm.model).await;
    let run = use_case.execute(&options, provenance, &StderrProgress).await?;
    let comparison = baseline.map(|baseline| comparer.execute(&run, &baseline, args.max_drop)).transpose()?;
    if args.json {
        let body = serde_json::json!({ "run": run, "comparison": comparison });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        print!("{}", render_run(&run, comparison.as_ref()));
    }
    let regressed = comparison.is_some_and(|c| c.regressed);
    Ok(ExitCode::from(if regressed { EXIT_EVAL_REGRESSION } else { EXIT_EVAL_OK }))
}

async fn import_run(context: &AppContext, run_id: i64, out: &std::path::Path, id: Option<String>) -> Result<ExitCode> {
    let db = context.paths.data_dir.join(HISTORY_DB_FILE);
    let feedback_store = Arc::new(SqliteFeedbackStore::open(&db)?);
    let source = HistoryRunSource::new(
        context.open_history()?,
        Arc::new(FeedbackService::new(feedback_store.clone())),
        Arc::new(CriterionVerifications::new(feedback_store)),
    );
    let use_case = ImportRun::new(Arc::new(source), Arc::new(GitChangeExporter), Arc::new(FsCaseRepository::new(out)));
    let imported = use_case.execute(run_id, id, &context.server().watch.base_branch).await?;
    println!(
        "Created {}/{} from run {run_id}: {} expected, {} forbidden, {} labelled criteria",
        out.display(),
        imported.id,
        imported.expected,
        imported.forbidden,
        imported.labelled_criteria
    );
    if imported.unlabelled_criteria > 0 {
        println!(
            "{} criteria have no `expect` yet and are not scored; edit {}/{}/case.toml to set expect = \"met\" or \"not_met\".",
            imported.unlabelled_criteria,
            out.display(),
            imported.id
        );
    }
    println!("Review case.toml before relying on it: findings you never judged are not labelled.");
    Ok(ExitCode::from(EXIT_EVAL_OK))
}

fn list(context: &AppContext) -> Result<ExitCode> {
    let runs = ListEvalRuns::new(open_store(context)?).execute(LISTED_RUNS)?;
    let runs: Vec<_> = runs.into_iter().map(|entry| entry.run).collect();
    print!("{}", render_list(&runs));
    Ok(ExitCode::from(EXIT_EVAL_OK))
}
