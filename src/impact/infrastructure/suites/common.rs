use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::impact::application::ImpactError;
use crate::impact::domain::TestRun;
use crate::sandbox::application::{ExecBackend, ExecRequest, ExecResult, FailureKind, MAX_OUTPUT_BYTES};

pub const RESULTS_DIR: &str = ".qgate-results";
const RUN_TIMEOUT_MS: u64 = 20 * 60 * 1000;
const INSTALL_TIMEOUT_MS: u64 = 15 * 60 * 1000;
const MAX_WALK_ENTRIES: usize = 50_000;
const MAX_WALK_DEPTH: usize = 12;
const MAX_REPORT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
const MTIME_SLACK: Duration = Duration::from_secs(2);
const SNIPPET_CHARS: usize = 600;
const TRUNCATION_PERCENT: usize = 98;
const SKIPPED_DIRS: &[&str] = &[".git", "node_modules", ".gradle", ".venv", ".qgate-venv", ".idea", "dist"];

static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn unique_name(prefix: &str, extension: &str) -> String {
    let n = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{RESULTS_DIR}/{prefix}-{}-{n}.{extension}", std::process::id())
}

pub struct SuiteExec {
    backend: Arc<dyn ExecBackend>,
}

impl SuiteExec {
    pub fn new(backend: Arc<dyn ExecBackend>) -> Self {
        Self { backend }
    }

    pub async fn run(&self, root: &Path, argv: Vec<String>) -> Result<ExecResult, ImpactError> {
        std::fs::create_dir_all(root.join(RESULTS_DIR))
            .map_err(|e| ImpactError::Failed(format!("cannot create {RESULTS_DIR}: {e}")))?;
        self.execute(root, argv, RUN_TIMEOUT_MS).await
    }

    pub async fn install(&self, root: &Path, argv: Vec<String>) -> Result<(), ImpactError> {
        let program = argv.join(" ");
        let result = self.execute(root, argv, INSTALL_TIMEOUT_MS).await?;
        if result.success() {
            return Ok(());
        }
        Err(ImpactError::Failed(format!("`{program}` failed: {}", snippet(&result.output_tail))))
    }

    async fn execute(&self, root: &Path, argv: Vec<String>, timeout_ms: u64) -> Result<ExecResult, ImpactError> {
        let env = BTreeMap::from([("CI".to_string(), "true".to_string())]);
        let request =
            ExecRequest { argv, cwd: root.to_path_buf(), env, timeout_ms, max_output_bytes: MAX_OUTPUT_BYTES };
        let result = self.backend.execute(request).await;
        match &result.failure {
            Some(f) if matches!(f.kind, FailureKind::NotFound | FailureKind::Unavailable) => {
                Err(ImpactError::ToolchainUnavailable(f.message.clone()))
            }
            Some(f) => Err(ImpactError::Failed(f.message.clone())),
            None => Ok(result),
        }
    }
}

pub fn finish(mut run: TestRun, result: &ExecResult) -> Result<TestRun, ImpactError> {
    if run.packages.is_empty() {
        return Err(ImpactError::NoResults(snippet(&result.output_tail)));
    }
    run.incomplete |= result.timed_out || looks_truncated(&result.output_tail);
    Ok(run)
}

pub fn looks_truncated(output: &str) -> bool {
    output.len() >= MAX_OUTPUT_BYTES / 100 * TRUNCATION_PERCENT
}

pub fn snippet(output: &str) -> String {
    let trimmed = output.trim();
    let skip = trimmed.chars().count().saturating_sub(SNIPPET_CHARS);
    trimmed.chars().skip(skip).collect()
}

pub fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

pub fn is_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

pub fn is_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

pub fn read_small(path: &Path, limit: u64) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

pub fn read_source(path: &Path) -> Option<String> {
    read_small(path, MAX_SOURCE_BYTES)
}

pub fn walk_files(root: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                return found;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() && depth < MAX_WALK_DEPTH && !is_skipped(&path) {
                stack.push((path, depth + 1));
            } else if kind.is_file() && keep(&path) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn is_skipped(dir: &Path) -> bool {
    dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| SKIPPED_DIRS.contains(&n))
}

pub fn fresh_reports(root: &Path, since: SystemTime, keep: impl Fn(&Path) -> bool) -> Vec<String> {
    let threshold = since.checked_sub(MTIME_SLACK).unwrap_or(since);
    walk_files(root, |path| keep(path) && modified_after(path, threshold))
        .iter()
        .filter_map(|path| read_small(path, MAX_REPORT_BYTES))
        .collect()
}

fn modified_after(path: &Path, threshold: SystemTime) -> bool {
    std::fs::symlink_metadata(path).and_then(|m| m.modified()).is_ok_and(|t| t >= threshold)
}

pub fn relative(root: &Path, path: &Path) -> String {
    let canonical = std::fs::canonicalize(root).ok();
    let stripped = path
        .strip_prefix(root)
        .ok()
        .or_else(|| canonical.as_deref().and_then(|c| path.strip_prefix(c).ok()))
        .unwrap_or(path);
    stripped.to_string_lossy().replace('\\', "/")
}

pub fn line_of(path: &Path, needle: &str) -> Option<u32> {
    if needle.trim().is_empty() {
        return None;
    }
    let source = read_source(path)?;
    let index = source.lines().position(|line| line.contains(needle))?;
    u32::try_from(index + 1).ok()
}

pub fn shell_quote(text: &str) -> String {
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=#,@+".contains(c)) {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub fn regex_escape(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, c| {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::application::fakes::RecordingBackend;

    #[test]
    fn walk_skips_dependency_dirs_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/node_modules/x")).unwrap();
        std::fs::create_dir_all(root.path().join("b")).unwrap();
        std::fs::write(root.path().join("a/node_modules/x/t.xml"), "x").unwrap();
        std::fs::write(root.path().join("b/t.xml"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("b/t.xml"), root.path().join("link.xml")).unwrap();
        let found = walk_files(root.path(), |p| p.extension().is_some_and(|e| e == "xml"));
        assert_eq!(found.iter().map(|p| relative(root.path(), p)).collect::<Vec<_>>(), vec!["b/t.xml"]);
    }

    #[test]
    fn fresh_reports_ignores_files_older_than_the_run() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("old.xml"), "old").unwrap();
        let future = SystemTime::now() + Duration::from_secs(60);
        assert!(fresh_reports(root.path(), future, |_| true).is_empty());
        let past = SystemTime::now() - Duration::from_secs(60);
        assert_eq!(fresh_reports(root.path(), past, |_| true), vec!["old"]);
    }

    #[test]
    fn relative_strips_the_root_even_through_a_symlinked_prefix() {
        let real = tempfile::tempdir().unwrap();
        let link_parent = tempfile::tempdir().unwrap();
        let link = link_parent.path().join("root");
        #[cfg(unix)]
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let canonical = std::fs::canonicalize(real.path()).unwrap();
        assert_eq!(relative(&link, &canonical.join("src/a.test.ts")), "src/a.test.ts");
        assert_eq!(relative(&link, &link.join("b.ts")), "b.ts");
        assert_eq!(relative(&link, Path::new("/elsewhere/c.ts")), "/elsewhere/c.ts");
    }

    #[test]
    fn helpers_quote_escape_and_locate() {
        assert_eq!(shell_quote("src/a.test.ts"), "src/a.test.ts");
        assert_eq!(shell_quote("it's ok"), r"'it'\''s ok'");
        assert_eq!(regex_escape("a(b)"), r"a\(b\)");
        assert_eq!(snippet(&"x".repeat(SNIPPET_CHARS + 5)).len(), SNIPPET_CHARS);
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("t.py"), "import x\n\ndef test_total():\n").unwrap();
        assert_eq!(line_of(&root.path().join("t.py"), "def test_total"), Some(3));
        assert_eq!(line_of(&root.path().join("t.py"), " "), None);
        assert!(unique_name("jest", "json").starts_with(".qgate-results/jest-"));
        assert_ne!(unique_name("a", "x"), unique_name("a", "x"));
    }

    #[test]
    fn finish_requires_results_and_flags_timeouts() {
        let result = ExecResult { output_tail: "compile error".into(), timed_out: true, ..ExecResult::default() };
        assert!(matches!(finish(TestRun::default(), &result), Err(ImpactError::NoResults(o)) if o == "compile error"));
        let mut run = TestRun::default();
        run.insert(
            "p",
            "t",
            crate::impact::domain::TestResult {
                outcome: crate::impact::domain::TestOutcome::Pass,
                output_tail: String::new(),
            },
        );
        assert!(finish(run, &result).unwrap().incomplete);
    }

    #[tokio::test]
    async fn exec_maps_missing_tools_and_failed_installs() {
        let root = tempfile::tempdir().unwrap();
        let missing =
            SuiteExec::new(Arc::new(RecordingBackend::answering(ExecResult::failed(FailureKind::NotFound, "no mvn"))));
        assert!(matches!(missing.run(root.path(), strings(&["mvn"])).await, Err(ImpactError::ToolchainUnavailable(_))));
        let failing = SuiteExec::new(Arc::new(RecordingBackend::answering(ExecResult {
            exit_code: Some(1),
            output_tail: "ERR lockfile".into(),
            ..ExecResult::default()
        })));
        let error = failing.install(root.path(), strings(&["npm", "ci"])).await.unwrap_err();
        assert!(error.to_string().contains("`npm ci` failed: ERR lockfile"));
        let backend = Arc::new(RecordingBackend::answering(ExecResult { exit_code: Some(0), ..ExecResult::default() }));
        SuiteExec::new(backend.clone()).run(root.path(), strings(&["x"])).await.unwrap();
        let request = &backend.requests.lock().unwrap()[0];
        assert_eq!(request.env.get("CI").map(String::as_str), Some("true"));
        assert!(is_dir(&root.path().join(RESULTS_DIR)), "the results dir exists before the tool runs");
    }
}
