//! Use case of the isolated runner: validate a request, confine its working directory to the work
//! root (after resolving symlinks), then execute it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ExecBackend;
use crate::sandbox::domain::{ExecRequest, ExecResult, FailureKind, is_inside, validate};

pub struct RunConfined {
    backend: Arc<dyn ExecBackend>,
    /// Canonical work root.
    root: PathBuf,
}

impl RunConfined {
    /// Fails when `root` does not exist.
    pub fn new(backend: Arc<dyn ExecBackend>, root: &Path) -> std::io::Result<Self> {
        Ok(Self { backend, root: std::fs::canonicalize(root)? })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn execute(&self, mut request: ExecRequest) -> ExecResult {
        if let Err(reason) = validate(&request) {
            return ExecResult::failed(FailureKind::Rejected, reason);
        }
        let cwd = match std::fs::canonicalize(&request.cwd) {
            Ok(cwd) if cwd.is_dir() => cwd,
            Ok(_) => return rejected_cwd(&request.cwd, "is not a directory"),
            Err(e) => return rejected_cwd(&request.cwd, &e.to_string()),
        };
        if !is_inside(&cwd, &self.root) {
            return rejected_cwd(&request.cwd, &format!("is outside the work root {}", self.root.display()));
        }
        request.cwd = cwd;
        self.backend.execute(request).await
    }
}

fn rejected_cwd(cwd: &Path, reason: &str) -> ExecResult {
    ExecResult::failed(FailureKind::Rejected, format!("working directory {} {reason}", cwd.display()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::sandbox::application::fakes::RecordingBackend;

    fn request(cwd: &Path) -> ExecRequest {
        ExecRequest {
            argv: vec!["true".into()],
            cwd: cwd.to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: 1000,
            max_output_bytes: 100,
        }
    }

    fn use_case(root: &Path) -> (Arc<RecordingBackend>, RunConfined) {
        let backend = Arc::new(RecordingBackend::answering(ExecResult { exit_code: Some(0), ..ExecResult::default() }));
        let use_case = RunConfined::new(backend.clone(), root).unwrap();
        (backend, use_case)
    }

    fn kind(result: &ExecResult) -> Option<FailureKind> {
        result.failure.as_ref().map(|f| f.kind)
    }

    #[tokio::test]
    async fn runs_below_the_root_with_the_canonical_cwd() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("snap")).unwrap();
        let (backend, use_case) = use_case(root.path());
        let result = use_case.execute(request(&root.path().join("snap/../snap"))).await;
        assert!(result.success());
        let sent = backend.requests.lock().unwrap()[0].cwd.clone();
        assert_eq!(sent, use_case.root().join("snap"));
    }

    #[tokio::test]
    async fn cwd_escapes_are_rejected_without_running() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("snap")).unwrap();
        std::fs::write(root.path().join("file"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let (backend, use_case) = use_case(root.path());

        let mut cases = vec![
            root.path().join("snap/../.."),
            root.path().to_path_buf(),
            outside.path().to_path_buf(),
            root.path().join("missing"),
            root.path().join("file"),
        ];
        if cfg!(unix) {
            cases.push(root.path().join("link"));
        }
        for cwd in cases {
            let result = use_case.execute(request(&cwd)).await;
            assert_eq!(kind(&result), Some(FailureKind::Rejected), "{}", cwd.display());
            assert!(result.failure.unwrap().message.contains("working directory"));
        }
        assert!(backend.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_requests_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (_, use_case) = use_case(root.path());
        let mut bad = request(root.path());
        bad.argv.clear();
        let result = use_case.execute(bad).await;
        assert_eq!(kind(&result), Some(FailureKind::Rejected));
        assert!(RunConfined::new(Arc::new(RecordingBackend::default()), &root.path().join("nope")).is_err());
    }
}
