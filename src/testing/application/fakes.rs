use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::llm::{
    ContentPart, LlmError, LoopLimits, LoopOutcome, LoopStop, SystemBlock, ToolHandler, ToolOutput, ToolSpec,
};
use crate::testing::domain::classify::RawRun;

use futures::future::BoxFuture;

use super::ports::{ScenarioSnapshot, ScenarioSnapshots, TestRunner, ToolLoop};

#[derive(Default)]
pub struct ScriptedLoop {
    pub steps: Vec<(String, Value)>,
    pub outputs: Mutex<Vec<ToolOutput>>,
    pub prompts: Mutex<Vec<String>>,
    pub fail_with: Option<LlmError>,
}

impl ScriptedLoop {
    pub fn new(steps: Vec<(&str, Value)>) -> Self {
        Self { steps: steps.into_iter().map(|(n, v)| (n.to_string(), v)).collect(), ..Self::default() }
    }
}

impl ToolLoop for ScriptedLoop {
    async fn drive<H: ToolHandler>(
        &self,
        system: &[SystemBlock],
        user: Vec<ContentPart>,
        tools: &[ToolSpec],
        handler: &H,
        limits: LoopLimits,
    ) -> Result<LoopOutcome, LlmError> {
        assert!(system.iter().all(|b| b.cache), "system prompt is cached");
        assert!(tools.iter().all(|t| t.input_schema["additionalProperties"] == false));
        for part in user {
            if let ContentPart::Text(text) = part {
                self.prompts.lock().unwrap().push(text);
            }
        }
        if let Some(error) = &self.fail_with {
            return Err(error.clone());
        }
        let mut turns = 0;
        for (name, input) in &self.steps {
            if turns >= limits.max_turns {
                return Ok(LoopOutcome { turns, stop: LoopStop::MaxTurns, final_text: String::new() });
            }
            turns += 1;
            let output = handler.call(name, input.clone()).await;
            self.outputs.lock().unwrap().push(output);
            if handler.finished() {
                return Ok(LoopOutcome { turns, stop: LoopStop::Finished, final_text: String::new() });
            }
        }
        Ok(LoopOutcome { turns, stop: LoopStop::NoMoreToolCalls, final_text: String::new() })
    }
}

#[derive(Default)]
pub struct FakeRunner {
    pub queue: Mutex<VecDeque<RawRun>>,
    pub calls: Mutex<Vec<(PathBuf, Vec<String>)>>,
}

impl FakeRunner {
    pub fn with(runs: Vec<RawRun>) -> Self {
        Self { queue: Mutex::new(runs.into()), ..Self::default() }
    }

    pub fn calls(&self) -> Vec<(PathBuf, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl TestRunner for FakeRunner {
    async fn run(&self, cwd: &Path, command: &[String], _timeout: Duration) -> RawRun {
        self.calls.lock().unwrap().push((cwd.to_path_buf(), command.to_vec()));
        self.queue.lock().unwrap().pop_front().unwrap_or(RawRun { exit_code: Some(0), ..RawRun::default() })
    }
}

pub fn failed_run(output: &str) -> RawRun {
    RawRun { exit_code: Some(1), output: output.into(), ..RawRun::default() }
}

pub const GO_ASSERTION_OUTPUT: &str = "--- FAIL: TestQgateS1 (0.00s)\n    qgate_s1_test.go:6: Add(2, 3) = -1, want 5\nFAIL\nFAIL\texample.com/m/calc\t0.01s";
pub const GO_BUILD_OUTPUT: &str =
    "# example.com/m/calc\ncalc/qgate_s1_test.go:6:9: undefined: Sum\nFAIL [build failed]";

pub struct CopySnapshots {
    pub source: PathBuf,
    pub created: Mutex<Vec<PathBuf>>,
}

impl CopySnapshots {
    pub fn of(source: &Path) -> Self {
        Self { source: source.to_path_buf(), created: Mutex::default() }
    }
}

impl ScenarioSnapshots for CopySnapshots {
    fn create<'a>(&'a self, _repo_root: &'a Path, _commit: &'a str) -> BoxFuture<'a, Result<ScenarioSnapshot, String>> {
        Box::pin(async move {
            if !self.source.is_dir() {
                return Err(format!("{} does not exist", self.source.display()));
            }
            let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
            copy_tree(&self.source, dir.path()).map_err(|e| e.to_string())?;
            self.created.lock().unwrap().push(dir.path().to_path_buf());
            Ok(ScenarioSnapshot::new(dir.path().to_path_buf(), dir))
        })
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
