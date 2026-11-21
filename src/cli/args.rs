use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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
    Runner(RunnerArgs),
}

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
    fn runner_takes_a_socket_and_defaults() {
        let cli = Cli::try_parse_from(["qgate", "runner", "--socket", "/work/.runner/r.sock", "--check"]).unwrap();
        let Command::Runner(args) = cli.command else { panic!("expected runner") };
        assert_eq!(args.socket, PathBuf::from("/work/.runner/r.sock"));
        assert!(args.check && args.max_jobs >= 1);
        assert!(Cli::try_parse_from(["qgate", "runner"]).is_err(), "--socket is required");
    }
}
