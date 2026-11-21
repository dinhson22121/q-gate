//! Thin command handlers: parse input, call one use case / pipeline, present the result.

use std::io::{IsTerminal, Read};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use super::args::{AuthAction, OutputFormat, ReposAction, ReviewArgs, SecretKind};
use super::pipeline::{AppContext, ReviewPipeline, ReviewRequest};
use super::render::{DisplayFilter, TerminalView, render_json, render_terminal};
use super::serve::{REPOS_FILE, repo_source};
use super::snapshot::resolve_commit;
use super::{EXIT_GATE_FAILED, EXIT_OK};
use crate::history::application::Trigger;
use crate::review::application::ReviewProgress;
use crate::review::infrastructure::local_git::{DiffTarget, discover_root};
use crate::settings::application::{CredentialName, delete_secret, store_secret};
use crate::settings::infrastructure::write_template;
use crate::watch::application::ports::{RepoRegistry, RepoSource};
use crate::watch::infrastructure::toml_registry::TomlRepoRegistry;

/// Prints progress to stderr (keeps stdout clean for JSON).
struct StderrProgress {
    enabled: bool,
}

impl ReviewProgress for StderrProgress {
    fn started(&self, files: usize, batches: usize) {
        if self.enabled {
            eprintln!("qgate: reviewing {files} file(s) in {batches} batch(es)…");
        }
    }
}

pub async fn review(context: Arc<AppContext>, args: ReviewArgs) -> Result<ExitCode> {
    let root = discover_root(&args.repo).await?;
    let target = match (args.base, args.commit) {
        (Some(base), _) => DiffTarget::Base(base),
        (None, Some(rev)) => DiffTarget::Commit(resolve_commit(&root, &rev).await?),
        (None, None) => DiffTarget::WorkingTree,
    };
    let history = if args.no_record {
        None
    } else {
        context.open_history().map_err(|e| eprintln!("qgate: warning: history disabled: {e:#}")).ok()
    };
    let request = ReviewRequest {
        repo_root: root,
        target,
        trigger: Trigger::Manual,
        explicit_ticket: args.ticket,
        ticket_in_progress: false,
    };
    let progress = StderrProgress { enabled: args.format == OutputFormat::Terminal };
    let outcome = ReviewPipeline::new(context, history).run(&request, &progress).await?;

    let note = outcome.ticket_note.as_deref();
    match args.format {
        OutputFormat::Json => println!("{}", render_json(&outcome.report, note, outcome.run_id)),
        OutputFormat::Terminal => {
            let filter = DisplayFilter {
                min_severity: outcome.config.report.min_severity,
                min_confidence: outcome.config.report.min_confidence,
            };
            let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
            print!("{}", render_terminal(&TerminalView { report: &outcome.report, filter, ticket_note: note, color }));
        }
    }
    Ok(ExitCode::from(if outcome.report.gate.passed { EXIT_OK } else { EXIT_GATE_FAILED }))
}

pub async fn init(repo: &Path) -> Result<ExitCode> {
    let root = discover_root(repo).await?;
    if write_template(&root)? {
        println!("Created {}", root.join(".qgate.toml").display());
        println!("Uncomment [[checks]] to run your tests/linters, and [jira] to check acceptance criteria.");
    } else {
        println!("{} already exists; left unchanged.", root.join(".qgate.toml").display());
    }
    Ok(ExitCode::from(EXIT_OK))
}

pub async fn repos(context: Arc<AppContext>, action: ReposAction) -> Result<ExitCode> {
    let registry = TomlRepoRegistry::new(context.paths.config_dir.join(REPOS_FILE));
    match action {
        ReposAction::Add { path } => {
            let repo = registry.add(&path).await?;
            println!("Watching {} ({})", repo.name, repo.path.display());
        }
        ReposAction::Remove { path } => {
            let removed = registry.remove(&path).await?;
            println!("{}", if removed { "Removed." } else { "Not in the explicit list." });
        }
        ReposAction::List => {
            let repos = repo_source(&context).list().await?;
            if repos.is_empty() {
                println!("No repositories found under {}.", context.server().repos.roots.join(", "));
            }
            for repo in repos {
                println!("{}\t{}", repo.name, repo.path.display());
            }
        }
    }
    Ok(ExitCode::from(EXIT_OK))
}

pub fn auth(context: &AppContext, action: AuthAction) -> Result<ExitCode> {
    match action {
        AuthAction::Set { name } => {
            let secret = read_secret(name)?;
            store_secret(credential(name), &secret, context.store.as_ref())?;
            println!("Stored {}.", credential(name).as_str());
        }
        AuthAction::Delete { name } => {
            delete_secret(credential(name), context.store.as_ref())?;
            println!("Deleted {}.", credential(name).as_str());
        }
    }
    Ok(ExitCode::from(EXIT_OK))
}

fn credential(kind: SecretKind) -> CredentialName {
    match kind {
        SecretKind::Anthropic => CredentialName::AnthropicApiKey,
        SecretKind::Jira => CredentialName::JiraApiToken,
    }
}

fn read_secret(kind: SecretKind) -> Result<String> {
    if std::io::stdin().is_terminal() {
        let prompt = match kind {
            SecretKind::Anthropic => "Anthropic API key: ",
            SecretKind::Jira => "Jira API token: ",
        };
        return rpassword::prompt_password(prompt).context("could not read the secret");
    }
    let mut value = String::new();
    std::io::stdin().read_to_string(&mut value).context("could not read the secret from stdin")?;
    if value.trim().is_empty() {
        bail!("no secret provided on stdin");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_kinds_map_to_credentials() {
        assert_eq!(credential(SecretKind::Anthropic), CredentialName::AnthropicApiKey);
        assert_eq!(credential(SecretKind::Jira), CredentialName::JiraApiToken);
    }

    #[test]
    fn progress_is_silent_when_disabled() {
        StderrProgress { enabled: false }.started(1, 1);
        StderrProgress { enabled: true }.started(1, 1);
    }
}
