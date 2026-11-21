//! `qgate doctor`: explains what is and is not set up, without printing secrets.

use std::process::ExitCode;
use std::time::Duration;

use super::pipeline::AppContext;
use super::serve::repo_source;
use super::{EXIT_ERROR, EXIT_OK};
use crate::settings::application::CredentialName;
use crate::settings::infrastructure::resolve_credential;
use crate::watch::application::ports::RepoSource;

/// Toolchain programs checks commonly need, with the flag that prints their version.
const TOOLCHAINS: &[(&str, &str)] = &[
    ("git", "--version"),
    ("go", "version"),
    ("java", "-version"),
    ("mvn", "--version"),
    ("gradle", "--version"),
    ("node", "--version"),
    ("python3", "--version"),
    ("cargo", "--version"),
];
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Line {
    level: Level,
    text: String,
}

pub async fn doctor(context: &AppContext) -> ExitCode {
    let mut lines = vec![
        Line { level: Level::Ok, text: format!("config dir {}", context.paths.config_dir.display()) },
        Line { level: Level::Ok, text: format!("data dir {}", context.paths.data_dir.display()) },
        credential_line(context, CredentialName::AnthropicApiKey, Level::Fail),
    ];
    if context.server().jira.enabled {
        lines.push(credential_line(context, CredentialName::JiraApiToken, Level::Fail));
    }
    lines.push(match context.open_history() {
        Ok(_) => Line { level: Level::Ok, text: "history database is writable".into() },
        Err(e) => Line { level: Level::Fail, text: format!("history database: {e:#}") },
    });
    lines.push(match repo_source(context).list().await {
        Ok(repos) if repos.is_empty() => Line {
            level: Level::Warn,
            text: format!("no git repositories found under {}", context.server().repos.roots.join(", ")),
        },
        Ok(repos) => Line { level: Level::Ok, text: format!("{} repositories found", repos.len()) },
        Err(e) => Line { level: Level::Fail, text: format!("repository discovery: {e}") },
    });
    for (program, flag) in TOOLCHAINS {
        lines.push(probe(program, flag).await);
    }
    print_lines(&lines);
    let failed = lines.iter().any(|l| l.level == Level::Fail);
    ExitCode::from(if failed { EXIT_ERROR } else { EXIT_OK })
}

fn credential_line(context: &AppContext, name: CredentialName, missing: Level) -> Line {
    match resolve_credential(name, context.store.as_ref()) {
        Ok(Some(_)) => Line { level: Level::Ok, text: format!("{} is configured", name.as_str()) },
        Ok(None) => Line { level: missing, text: format!("{} is missing (set {})", name.as_str(), name.env_var()) },
        Err(e) => Line { level: Level::Fail, text: format!("{}: {e}", name.as_str()) },
    }
}

/// Missing toolchains are warnings: only the ones your `[[checks]]` use matter.
async fn probe(program: &str, flag: &str) -> Line {
    let run = tokio::process::Command::new(program).arg(flag).kill_on_drop(true).output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(output)) if output.status.success() => {
            let text = if output.stdout.is_empty() { &output.stderr } else { &output.stdout };
            let version = String::from_utf8_lossy(text).lines().next().unwrap_or_default().trim().to_string();
            Line { level: Level::Ok, text: format!("{program}: {version}") }
        }
        _ => Line { level: Level::Warn, text: format!("{program}: not installed") },
    }
}

fn print_lines(lines: &[Line]) {
    for line in lines {
        let symbol = match line.level {
            Level::Ok => "✔",
            Level::Warn => "!",
            Level::Fail => "✘",
        };
        println!("{symbol} {}", line.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_reports_installed_and_missing_programs() {
        assert_eq!(probe("git", "--version").await.level, Level::Ok);
        let missing = probe("qgate-definitely-missing-program", "--version").await;
        assert_eq!(missing.level, Level::Warn);
        assert!(missing.text.contains("not installed"));
    }
}
