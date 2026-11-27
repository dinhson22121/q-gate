use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::OnceCell;

use crate::cache::application::digest;
use crate::conventions::infrastructure::prompt_material as conventions_material;
use crate::history::application::RunProvenance;
use crate::review::infrastructure::claude::prompt_material as review_material;
use crate::testing::application::prompt_fingerprint as testing_fingerprint;
use crate::tickets::infrastructure::prompt_material as tickets_material;

const PROMPT_HASH_VERSION: &str = "1";
const UNKNOWN_VERSION: &str = "unknown";
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

static PROMPT_HASH: OnceLock<String> = OnceLock::new();
static CLI_VERSION: OnceCell<String> = OnceCell::const_new();

fn combine_prompts<S: AsRef<[u8]>>(parts: &[S]) -> String {
    digest(std::iter::once(PROMPT_HASH_VERSION.as_bytes()).chain(parts.iter().map(AsRef::as_ref)))
}

pub fn prompt_hash() -> String {
    PROMPT_HASH
        .get_or_init(|| {
            let mut parts = review_material();
            parts.extend(tickets_material());
            parts.extend(conventions_material());
            parts.push(testing_fingerprint());
            combine_prompts(&parts)
        })
        .clone()
}

pub async fn claude_cli_version(bin: &Path) -> String {
    CLI_VERSION.get_or_init(|| read_cli_version(bin)).await.clone()
}

async fn read_cli_version(bin: &Path) -> String {
    let run = tokio::process::Command::new(bin).arg("--version").kill_on_drop(true).output();
    match tokio::time::timeout(VERSION_TIMEOUT, run).await {
        Ok(Ok(output)) if output.status.success() => parse_version(&String::from_utf8_lossy(&output.stdout)),
        _ => UNKNOWN_VERSION.into(),
    }
}

fn parse_version(stdout: &str) -> String {
    let line = stdout.lines().next().unwrap_or_default().trim();
    if line.is_empty() { UNKNOWN_VERSION.into() } else { line.into() }
}

pub async fn current_provenance(model: &str, claude_bin: &Path) -> RunProvenance {
    RunProvenance {
        model: model.trim().to_string(),
        claude_cli_version: claude_cli_version(claude_bin).await,
        prompt_hash: prompt_hash(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_hash_is_stable_64_hex() {
        let hash = prompt_hash();
        assert_eq!(hash, prompt_hash());
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn combined_hash_changes_when_any_prompt_changes() {
        let base = combine_prompts(&["review", "audit", "criteria"]);
        assert_eq!(base, combine_prompts(&["review", "audit", "criteria"]));
        assert_ne!(base, combine_prompts(&["review", "audit", "criteria!"]));
        assert_ne!(base, combine_prompts(&["review", "audit"]));
        assert_ne!(base, combine_prompts(&["reviewaudit", "criteria"]));
    }

    #[test]
    fn version_is_the_trimmed_first_line_or_unknown() {
        assert_eq!(parse_version("  2.1.0 (Claude Code)\nextra\n"), "2.1.0 (Claude Code)");
        assert_eq!(parse_version("\n"), UNKNOWN_VERSION);
    }

    #[tokio::test]
    async fn unreadable_binary_reports_unknown() {
        assert_eq!(read_cli_version(Path::new("/definitely/missing/claude")).await, UNKNOWN_VERSION);
    }

    #[tokio::test]
    async fn a_working_binary_reports_its_version() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("claude");
        std::fs::write(&fake, "#!/bin/sh\necho '9.9.9 (Claude Code)'\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(read_cli_version(&fake).await, "9.9.9 (Claude Code)");
    }
}
