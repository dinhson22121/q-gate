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
    /// Run the background service: review every new commit and serve the dashboard.
    Serve,
    /// Review changes now (uncommitted changes by default).
    Review(ReviewArgs),
    /// Create a commented .qgate.toml in a repository.
    Init {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Check that qgate is set up correctly.
    Doctor,
    /// Manage explicitly watched repositories (repos under the scan roots are found automatically).
    Repos {
        #[command(subcommand)]
        action: ReposAction,
    },
    /// Store or delete credentials for native use (in Docker, use environment variables instead).
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Print the dashboard login link (in Docker: `docker compose exec qgate qgate login-url`).
    LoginUrl,
    /// Run untrusted commands (repo checks, generated tests) for `qgate serve` in an isolated container.
    Runner(RunnerArgs),
}

#[derive(Debug, clap::Args)]
pub struct RunnerArgs {
    /// Unix socket to listen on (the qgate service connects via QGATE_RUNNER_SOCKET).
    #[arg(long)]
    pub socket: PathBuf,
    /// Only commands whose working directory is below this directory are run [default: $QGATE_WORK_DIR, else /work].
    #[arg(long)]
    pub work_root: Option<PathBuf>,
    /// Commands run at the same time.
    #[arg(long, default_value_t = 4)]
    pub max_jobs: usize,
    /// Ping a running runner on --socket and exit (container health check).
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, clap::Args)]
pub struct ReviewArgs {
    /// Repository (or any directory inside it).
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Review everything since the merge-base with this branch, e.g. `main`.
    #[arg(long, conflicts_with = "commit")]
    pub base: Option<String>,
    /// Review a single commit.
    #[arg(long)]
    pub commit: Option<String>,
    /// Jira ticket to check acceptance criteria against (default: detected from branch / commit message).
    #[arg(long)]
    pub ticket: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Terminal)]
    pub format: OutputFormat,
    /// Do not save this run to the dashboard history.
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
    /// Watch a repository that is outside the scan roots.
    Add { path: PathBuf },
    /// Stop explicitly watching a repository.
    Remove { path: PathBuf },
    /// List watched repositories (explicit and discovered).
    List,
}

#[derive(Debug, Subcommand)]
pub enum AuthAction {
    /// Store a secret (read from a hidden prompt, or from stdin when piped).
    Set { name: SecretKind },
    /// Delete a stored secret.
    Delete { name: SecretKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SecretKind {
    Anthropic,
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
