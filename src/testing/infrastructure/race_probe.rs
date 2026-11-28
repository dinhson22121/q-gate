use std::path::Path;
use std::time::Duration;

use futures::future::BoxFuture;

use crate::testing::application::ports::{RaceDetectorProbe, TestRunner};
use crate::testing::domain::race::cgo_compiler;

use super::process::ExecTestRunner;

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ExecRaceDetectorProbe {
    runner: ExecTestRunner,
}

impl ExecRaceDetectorProbe {
    pub fn new(runner: ExecTestRunner) -> Self {
        Self { runner }
    }

    async fn probe(&self, cwd: &Path) -> bool {
        let go_env = ["go", "env", "CGO_ENABLED", "CC"].map(String::from);
        let env = self.runner.run(cwd, &go_env, PROBE_TIMEOUT).await;
        if env.exit_code != Some(0) {
            return false;
        }
        let Some(compiler) = cgo_compiler(&env.output) else {
            return false;
        };
        let version = self.runner.run(cwd, &[compiler, "--version".to_string()], PROBE_TIMEOUT).await;
        version.exit_code == Some(0)
    }
}

impl RaceDetectorProbe for ExecRaceDetectorProbe {
    fn available<'a>(&'a self, cwd: &'a Path) -> BoxFuture<'a, bool> {
        Box::pin(self.probe(cwd))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::sandbox::application::{ExecBackend, ExecRequest, ExecResult, FailureKind};

    struct Scripted {
        answers: Mutex<VecDeque<ExecResult>>,
        argv: Mutex<Vec<Vec<String>>>,
    }

    impl Scripted {
        fn answering(answers: Vec<ExecResult>) -> Arc<Self> {
            Arc::new(Self { answers: Mutex::new(answers.into()), argv: Mutex::default() })
        }
    }

    impl ExecBackend for Scripted {
        fn execute(&self, request: ExecRequest) -> BoxFuture<'_, ExecResult> {
            self.argv.lock().unwrap().push(request.argv);
            let answer = self.answers.lock().unwrap().pop_front().unwrap_or_default();
            Box::pin(async move { answer })
        }
    }

    fn ok(output: &str) -> ExecResult {
        ExecResult { exit_code: Some(0), output_tail: output.into(), ..ExecResult::default() }
    }

    async fn probe(answers: Vec<ExecResult>) -> (bool, Vec<Vec<String>>) {
        let backend = Scripted::answering(answers);
        let probe = ExecRaceDetectorProbe::new(ExecTestRunner::new(backend.clone()));
        let available = probe.available(Path::new("/work/s1")).await;
        let argv = backend.argv.lock().unwrap().clone();
        (available, argv)
    }

    #[tokio::test]
    async fn available_when_cgo_is_on_and_the_compiler_runs() {
        let (available, argv) = probe(vec![ok("1\ngcc\n"), ok("gcc 12")]).await;
        assert!(available);
        assert_eq!(argv[0], ["go", "env", "CGO_ENABLED", "CC"]);
        assert_eq!(argv[1], ["gcc", "--version"]);
    }

    #[tokio::test]
    async fn unavailable_when_cgo_is_off_without_probing_the_compiler() {
        let (available, argv) = probe(vec![ok("0\ngcc\n")]).await;
        assert!(!available);
        assert_eq!(argv.len(), 1);
    }

    #[tokio::test]
    async fn unavailable_when_the_compiler_is_missing_or_go_fails() {
        let missing = ExecResult::failed(FailureKind::NotFound, "gcc: not found");
        assert!(!probe(vec![ok("1\ngcc\n"), missing]).await.0);
        let broken = ExecResult { exit_code: Some(2), ..ExecResult::default() };
        assert!(!probe(vec![broken]).await.0);
    }
}
