use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::eval::application::{BaselineRef, DEFAULT_MAX_DROP};
use crate::llm::is_pinned_model_id;
use crate::settings::application::Effort;

#[derive(Debug, Parser)]
#[command(
    name = "qgate",
    version,
    about = "Verify code changes with deterministic checks, Jira acceptance criteria and Claude before they reach production"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Serve,
    Review(ReviewArgs),
    Init {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    Doctor,
    Repos {
        #[command(subcommand)]
        action: ReposAction,
    },
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    LoginUrl,
    Ticket {
        key: String,
    },
    Runner(RunnerArgs),
    Eval(EvalArgs),
}

#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct EvalArgs {
    #[command(subcommand)]
    pub action: Option<EvalAction>,
    #[command(flatten)]
    pub run: EvalRunArgs,
}

#[derive(Debug, clap::Args)]
pub struct EvalRunArgs {
    #[arg(long)]
    pub cases: Option<PathBuf>,
    #[arg(long = "only", value_name = "CASE_ID")]
    pub only: Vec<String>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=10))]
    pub runs: u32,
    #[arg(long)]
    pub with_tests: bool,
    #[arg(long, value_name = "latest|ID")]
    pub baseline: Option<BaselineRef>,
    #[arg(long, default_value_t = DEFAULT_MAX_DROP)]
    pub max_drop: f64,
    #[arg(long)]
    pub json: bool,
    #[arg(long, value_parser = parse_pinned_model)]
    pub model: Option<String>,
    #[arg(long)]
    pub effort: Option<Effort>,
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

fn parse_pinned_model(value: &str) -> Result<String, String> {
    if is_pinned_model_id(value) {
        Ok(value.to_string())
    } else {
        Err(format!("`{value}` is not a full, pinned model id such as claude-opus-5-5 (aliases are not allowed)"))
    }
}

#[derive(Debug, Subcommand)]
pub enum EvalAction {
    ImportRun {
        run_id: i64,
        #[arg(long, default_value = DEFAULT_CASES_DIR)]
        out: PathBuf,
        #[arg(long)]
        id: Option<String>,
    },
    List,
}

pub const DEFAULT_CASES_DIR: &str = "eval/cases";
pub const IMAGE_CASES_DIR: &str = "/usr/share/qgate/eval/cases";

#[derive(Debug, clap::Args)]
pub struct RunnerArgs {
    #[arg(long)]
    pub socket: PathBuf,
    #[arg(long)]
    pub work_root: Option<PathBuf>,
    #[arg(long, default_value_t = 4)]
    pub max_jobs: usize,
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, clap::Args)]
pub struct ReviewArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long, conflicts_with = "commit")]
    pub base: Option<String>,
    #[arg(long)]
    pub commit: Option<String>,
    #[arg(long)]
    pub ticket: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Terminal)]
    pub format: OutputFormat,
    #[arg(long)]
    pub no_record: bool,
    #[arg(long, conflicts_with_all = ["base", "commit"])]
    pub working_tree: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Terminal,
    Json,
}

#[derive(Debug, Subcommand)]
pub enum ReposAction {
    Add { path: PathBuf },
    Remove { path: PathBuf },
    List,
}

#[derive(Debug, Subcommand)]
pub enum AuthAction {
    Set { name: SecretKind },
    Delete { name: SecretKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SecretKind {
    Jira,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn base_and_commit_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["qgate", "review", "--base", "main", "--commit", "abc"]).is_err());
        let cli = Cli::try_parse_from(["qgate", "review", "--commit", "abc", "--format", "json"]).unwrap();
        let Command::Review(args) = cli.command else { panic!("expected review") };
        assert_eq!(args.commit.as_deref(), Some("abc"));
        assert_eq!(args.format, OutputFormat::Json);
    }

    #[test]
    fn eval_defaults_to_one_run_over_the_shipped_cases() {
        let cli = Cli::try_parse_from(["qgate", "eval"]).unwrap();
        let Command::Eval(args) = cli.command else { panic!("expected eval") };
        assert!(args.action.is_none());
        assert_eq!(args.run.cases, None);
        assert_eq!((args.run.runs, args.run.with_tests, args.run.json), (1, false, false));
        assert_eq!(args.run.max_drop, DEFAULT_MAX_DROP);
        assert!(args.run.baseline.is_none() && args.run.only.is_empty());
    }

    #[test]
    fn eval_flags_parse() {
        let cli = Cli::try_parse_from([
            "qgate",
            "eval",
            "--only",
            "a",
            "--only",
            "b",
            "--runs",
            "3",
            "--with-tests",
            "--baseline",
            "latest",
            "--max-drop",
            "0.1",
            "--json",
        ])
        .unwrap();
        let Command::Eval(args) = cli.command else { panic!("expected eval") };
        assert_eq!(args.run.only, ["a", "b"]);
        assert_eq!((args.run.runs, args.run.with_tests, args.run.json, args.run.max_drop), (3, true, true, 0.1));
        assert_eq!(args.run.baseline, Some(BaselineRef::Latest));
        let by_id = Cli::try_parse_from(["qgate", "eval", "--baseline", "7"]).unwrap();
        let Command::Eval(args) = by_id.command else { panic!("expected eval") };
        assert_eq!(args.run.baseline, Some(BaselineRef::Id(7)));
    }

    #[test]
    fn eval_rejects_bad_runs_and_baselines() {
        for bad in [["--runs", "0"], ["--runs", "11"], ["--baseline", "nope"]] {
            assert!(Cli::try_parse_from(["qgate", "eval", bad[0], bad[1]]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn eval_subcommands_parse() {
        let import =
            Cli::try_parse_from(["qgate", "eval", "import-run", "42", "--out", "/tmp/c", "--id", "my-case"]).unwrap();
        let Command::Eval(args) = import.command else { panic!("expected eval") };
        let Some(EvalAction::ImportRun { run_id, out, id }) = args.action else { panic!("expected import-run") };
        assert_eq!((run_id, out, id.as_deref()), (42, PathBuf::from("/tmp/c"), Some("my-case")));
        let list = Cli::try_parse_from(["qgate", "eval", "list"]).unwrap();
        let Command::Eval(args) = list.command else { panic!("expected eval") };
        assert!(matches!(args.action, Some(EvalAction::List)));
        assert!(Cli::try_parse_from(["qgate", "eval", "list", "--runs", "2"]).is_err());
    }

    #[test]
    fn runner_takes_a_socket_and_defaults() {
        let cli = Cli::try_parse_from(["qgate", "runner", "--socket", "/work/.runner/r.sock", "--check"]).unwrap();
        let Command::Runner(args) = cli.command else { panic!("expected runner") };
        assert_eq!(args.socket, PathBuf::from("/work/.runner/r.sock"));
        assert!(args.check && args.max_jobs >= 1);
        assert!(Cli::try_parse_from(["qgate", "runner"]).is_err(), "--socket is required");
    }
}
