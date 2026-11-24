use std::process::ExitCode;
use std::time::Duration;

use super::pipeline::AppContext;
use super::serve::repo_source;
use super::{EXIT_ERROR, EXIT_OK};
use crate::watch::application::ports::RepoSource;

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
    ];
    lines.push(claude_line(context).await);
    if context.server().jira.enabled {
        lines.push(Line { level: Level::Ok, text: "jira provider: Atlassian Remote MCP (OAuth)".into() });
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

async fn claude_line(context: &AppContext) -> Line {
    probe_claude(&context.cli_config("", "").claude_bin).await
}

async fn probe_claude(bin: &std::path::Path) -> Line {
    let run = tokio::process::Command::new(bin).arg("--version").kill_on_drop(true).output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(output)) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Line {
                level: Level::Ok,
                text: format!("claude CLI {version} (log in with `claude login` or set CLAUDE_CODE_OAUTH_TOKEN)"),
            }
        }
        _ => Line {
            level: Level::Fail,
            text: format!("claude CLI not found at `{}` (install Claude Code or set llm.claude_bin)", bin.display()),
        },
    }
}

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
    async fn claude_probe_fails_for_a_missing_binary_and_passes_for_a_working_one() {
        let missing = probe_claude(std::path::Path::new("/definitely/missing/claude")).await;
        assert_eq!(missing.level, Level::Fail);
        assert!(missing.text.contains("llm.claude_bin"));
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("claude");
        std::fs::write(&fake, "#!/bin/sh\necho '9.9.9 (Claude Code)'\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let found = probe_claude(&fake).await;
        assert_eq!(found.level, Level::Ok);
        assert!(found.text.contains("9.9.9") && found.text.contains("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[tokio::test]
    async fn probe_reports_installed_and_missing_programs() {
        assert_eq!(probe("git", "--version").await.level, Level::Ok);
        let missing = probe("qgate-definitely-missing-program", "--version").await;
        assert_eq!(missing.level, Level::Warn);
        assert!(missing.text.contains("not installed"));
    }
}
