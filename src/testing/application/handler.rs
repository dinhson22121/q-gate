//! The tools the test agent may call for ONE scenario. Every fact that ends up in the evidence
//! (test path, source, command, exit code, output, code references) is recorded here, not taken
//! from the model.

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::llm::{ToolHandler, ToolOutput, ToolSpec};
use crate::review::application::{EvidenceRef, TestVerdict};
use crate::testing::domain::classify::classify;
use crate::testing::domain::language::TestLanguage;
use crate::testing::domain::paths::normalize_relative;
use crate::testing::domain::profile::TestTarget;
use crate::testing::domain::verdict::RunRecord;

use super::ports::{TestRunner, Workspace};

pub const LIST_FILES: &str = "list_files";
pub const READ_FILE: &str = "read_file";
pub const SEARCH: &str = "search";
pub const WRITE_TEST: &str = "write_test";
pub const RUN_TEST: &str = "run_test";
pub const REPORT_RESULT: &str = "report_result";

const MAX_READ_LINES: usize = 2000;
const MAX_READ_BYTES: usize = 200 * 1024;
const MAX_LIST_ENTRIES: usize = 500;
const MAX_SEARCH_HITS: usize = 100;
const MAX_TEST_SOURCE_BYTES: usize = 200 * 1024;
/// A quoted line may be this many lines away from the reported line.
const QUOTE_LINE_TOLERANCE: u32 = 3;
/// `report_result` calls with no verifiable code reference that are bounced back for correction.
const MAX_REJECTED_REPORTS: u32 = 1;

/// Limits of one scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandlerLimits {
    pub language: TestLanguage,
    pub timeout: Duration,
    /// Extra runs after the first one.
    pub max_repair_rounds: u32,
}

/// The model's final report, after verification of its code references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub advisory: TestVerdict,
    pub code_under_test: Vec<EvidenceRef>,
    /// References that did not match the snapshot, with the reason.
    pub rejected: Vec<String>,
    pub explanation: String,
}

/// What the handler recorded, for the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerFacts {
    pub target: Option<TestTarget>,
    pub runs: Vec<RunRecord>,
    pub report: Option<Report>,
}

#[derive(Default)]
struct State {
    /// Index into `targets` once a test was written.
    chosen: Option<usize>,
    source: Option<String>,
    runs: Vec<RunRecord>,
    running: bool,
    report: Option<Report>,
    rejected_reports: u32,
}

pub struct ScenarioToolHandler<'a, W, R> {
    workspace: &'a W,
    runner: &'a R,
    targets: &'a [TestTarget],
    limits: HandlerLimits,
    state: Mutex<State>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListFilesInput {
    dir: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileInput {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    pattern: String,
    glob: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteTestInput {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunTestInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportInput {
    verdict: TestVerdict,
    code_under_test: Vec<EvidenceRef>,
    explanation: String,
}

impl<'a, W: Workspace, R: TestRunner> ScenarioToolHandler<'a, W, R> {
    pub fn new(workspace: &'a W, runner: &'a R, targets: &'a [TestTarget], limits: HandlerLimits) -> Self {
        Self { workspace, runner, targets, limits, state: Mutex::new(State::default()) }
    }

    pub fn into_facts(self) -> HandlerFacts {
        let state = self.state.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
        HandlerFacts { target: state.chosen.map(|i| self.targets[i].clone()), runs: state.runs, report: state.report }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn dispatch(&self, name: &str, input: Value) -> ToolOutput {
        let result = match name {
            LIST_FILES => parse(input).and_then(|i| self.list_files(&i)),
            READ_FILE => parse(input).and_then(|i| self.read_file(&i)),
            SEARCH => parse(input).and_then(|i| self.search(&i)),
            WRITE_TEST => parse(input).and_then(|i| self.write_test(&i)),
            RUN_TEST => match parse::<RunTestInput>(input) {
                Ok(_) => self.run_test().await,
                Err(e) => Err(e),
            },
            REPORT_RESULT => parse(input).and_then(|i| self.report_result(i)),
            other => Err(format!("unknown tool '{other}'")),
        };
        result.map_or_else(ToolOutput::error, ToolOutput::ok)
    }

    fn list_files(&self, input: &ListFilesInput) -> Result<String, String> {
        let mut entries = self.workspace.list_dir(&input.dir)?;
        let total = entries.len();
        entries.truncate(MAX_LIST_ENTRIES);
        let mut text = entries.join("\n");
        if total > MAX_LIST_ENTRIES {
            text.push_str(&format!("\n... {} more entries", total - MAX_LIST_ENTRIES));
        }
        Ok(text)
    }

    fn read_file(&self, input: &ReadFileInput) -> Result<String, String> {
        let text = self.workspace.read_text(&input.path)?;
        let lines: Vec<&str> = text.lines().collect();
        let start = input.start_line.unwrap_or(1).max(1);
        let end = input.end_line.unwrap_or(lines.len()).min(lines.len()).min(start + MAX_READ_LINES - 1);
        let mut out = format!("{} (lines {start}-{end} of {})\n", input.path, lines.len());
        for (index, line) in lines.iter().enumerate().take(end).skip(start - 1) {
            let numbered = format!("{}\t{line}\n", index + 1);
            if out.len() + numbered.len() > MAX_READ_BYTES {
                out.push_str("... truncated at 200 KB; read the rest with start_line\n");
                break;
            }
            out.push_str(&numbered);
        }
        Ok(out)
    }

    fn search(&self, input: &SearchInput) -> Result<String, String> {
        if input.pattern.is_empty() {
            return Err("pattern must not be empty".into());
        }
        let hits = self.workspace.search(&input.pattern, input.glob.as_deref(), MAX_SEARCH_HITS)?;
        if hits.is_empty() {
            return Ok("no matches".into());
        }
        let mut text: Vec<String> = hits.iter().map(|h| format!("{}:{}: {}", h.file, h.line, h.text)).collect();
        if hits.len() >= MAX_SEARCH_HITS {
            text.push(format!("... stopped at {MAX_SEARCH_HITS} matches; narrow the pattern or glob"));
        }
        Ok(text.join("\n"))
    }

    fn write_test(&self, input: &WriteTestInput) -> Result<String, String> {
        let path = normalize_relative(&input.path).map_err(|e| e.to_string())?;
        if input.content.trim().is_empty() {
            return Err("content must not be empty".into());
        }
        if input.content.len() > MAX_TEST_SOURCE_BYTES {
            return Err("test source is larger than 200 KB".into());
        }
        let mut state = self.lock();
        if state.running {
            return Err("a test run is in progress".into());
        }
        if state.runs.len() > self.limits.max_repair_rounds as usize {
            return Err("no test runs left; call report_result".into());
        }
        let index = match state.chosen {
            Some(index) if self.targets[index].test_file == path => index,
            Some(index) => {
                return Err(format!(
                    "only one test file per scenario: keep writing to {}",
                    self.targets[index].test_file
                ));
            }
            None => {
                let index = self.targets.iter().position(|t| t.test_file == path).ok_or_else(|| {
                    format!("'{path}' is not an allowed test path; use one of: {}", self.allowed_paths())
                })?;
                if self.workspace.has_path(&path) {
                    return Err(format!("'{path}' already exists; qgate only writes new test files"));
                }
                index
            }
        };
        let create_new = state.chosen.is_none();
        self.workspace.write_file(&path, &input.content, create_new)?;
        state.chosen = Some(index);
        state.source = Some(input.content.clone());
        Ok(format!("wrote {path} ({} bytes); run it with run_test", input.content.len()))
    }

    fn allowed_paths(&self) -> String {
        self.targets.iter().map(|t| t.test_file.as_str()).collect::<Vec<_>>().join(", ")
    }

    async fn run_test(&self) -> Result<String, String> {
        let (target, source) = {
            let mut state = self.lock();
            let (Some(index), Some(source)) = (state.chosen, state.source.clone()) else {
                return Err("write the test with write_test first".into());
            };
            if state.running {
                return Err("a test run is already in progress".into());
            }
            if state.runs.last().is_some_and(|r| r.class.is_terminal()) {
                return Err("the test cannot run in this environment; call report_result".into());
            }
            if state.runs.len() > self.limits.max_repair_rounds as usize {
                return Err("no test runs left; call report_result".into());
            }
            state.running = true;
            (&self.targets[index], source)
        };
        let cwd = self.workspace.root().join(&target.module_dir);
        let raw = self.runner.run(&cwd, &target.command, self.limits.timeout).await;
        let class = classify(self.limits.language, &raw);
        let mut state = self.lock();
        state.running = false;
        let runs_left = (self.limits.max_repair_rounds as usize + 1).saturating_sub(state.runs.len() + 1);
        let reply = json!({
            "command": target.command.join(" "),
            "exit_code": raw.exit_code,
            "classification": class.as_str(),
            "runs_left": runs_left,
            "output": raw.output,
        });
        state.runs.push(RunRecord { class, raw, source });
        Ok(reply.to_string())
    }

    fn report_result(&self, input: ReportInput) -> Result<String, String> {
        let test_file = {
            let state = self.lock();
            if state.report.is_some() {
                return Err("result already reported".into());
            }
            state.chosen.map(|i| self.targets[i].test_file.clone())
        };
        let (valid, rejected): (Vec<_>, Vec<_>) = input
            .code_under_test
            .into_iter()
            .map(|r| self.verify_ref(r, test_file.as_deref()))
            .partition(Result::is_ok);
        let valid: Vec<EvidenceRef> = valid.into_iter().filter_map(Result::ok).collect();
        let rejected: Vec<String> = rejected.into_iter().filter_map(Result::err).collect();
        let mut state = self.lock();
        if valid.is_empty() && state.rejected_reports < MAX_REJECTED_REPORTS {
            state.rejected_reports += 1;
            return Err(format!(
                "no code_under_test reference matches the snapshot ({}); quote production lines exactly as \
                 read_file shows them and call report_result again",
                rejected.join("; ")
            ));
        }
        state.report =
            Some(Report { advisory: input.verdict, code_under_test: valid, rejected, explanation: input.explanation });
        Ok("result recorded".into())
    }

    /// The quote must be on `line` (±3) of a production file in the snapshot.
    fn verify_ref(&self, reference: EvidenceRef, test_file: Option<&str>) -> Result<EvidenceRef, String> {
        let describe = |why: &str| format!("{}:{} {why}", reference.file, reference.line);
        let path = normalize_relative(&reference.file).map_err(|e| describe(&e.to_string()))?;
        if Some(path.as_str()) == test_file {
            return Err(describe("is the generated test, not production code"));
        }
        let quote = reference.quote.trim();
        if quote.is_empty() || reference.line == 0 {
            return Err(describe("needs a line number and a non-empty quote"));
        }
        let text = self.workspace.read_text(&path).map_err(|e| describe(&e))?;
        let first = reference.line.saturating_sub(QUOTE_LINE_TOLERANCE).max(1) as usize;
        let last = reference.line.saturating_add(QUOTE_LINE_TOLERANCE) as usize;
        let found = text.lines().enumerate().take(last).skip(first - 1).any(|(_, line)| line.contains(quote));
        if !found {
            return Err(describe("does not contain the quote"));
        }
        Ok(EvidenceRef { file: path, line: reference.line, quote: quote.to_string() })
    }
}

impl<W: Workspace, R: TestRunner> ToolHandler for ScenarioToolHandler<'_, W, R> {
    fn call(&self, name: &str, input: Value) -> impl Future<Output = ToolOutput> + Send {
        let name = name.to_string();
        async move { self.dispatch(&name, input).await }
    }

    fn finished(&self) -> bool {
        self.lock().report.is_some()
    }
}

fn parse<T: DeserializeOwned>(input: Value) -> Result<T, String> {
    serde_json::from_value(input).map_err(|e| format!("invalid input: {e}"))
}

/// The tool contracts (strict object schemas).
pub fn tool_specs() -> Vec<ToolSpec> {
    let spec = |name: &str, description: &str, properties: Value, required: &[&str]| ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        }),
    };
    let path = json!({ "type": "string", "description": "Path relative to the repository root." });
    vec![
        spec(
            LIST_FILES,
            "List the entries of a directory (directories end with '/').",
            json!({ "dir": path }),
            &["dir"],
        ),
        spec(
            READ_FILE,
            "Read a text file with line numbers (at most 2000 lines / 200 KB per call).",
            json!({
                "path": path,
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
            }),
            &["path"],
        ),
        spec(
            SEARCH,
            "Find lines containing a literal string (not a regex), optionally only in files matching a glob.",
            json!({
                "pattern": { "type": "string", "minLength": 1 },
                "glob": { "type": "string", "description": "e.g. **/*.go" },
            }),
            &["pattern"],
        ),
        spec(
            WRITE_TEST,
            "Write the scenario's single test file. The path must be one of the allowed test paths; later \
             calls may only rewrite the same file.",
            json!({ "path": path, "content": { "type": "string" } }),
            &["path", "content"],
        ),
        spec(RUN_TEST, "Run the written test with qgate's fixed command.", json!({}), &[]),
        spec(
            REPORT_RESULT,
            "Finish: report your verdict and the production code the test exercises.",
            json!({
                "verdict": { "type": "string", "enum": ["passed", "failed_assertion", "inconclusive"] },
                "code_under_test": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "file": { "type": "string" },
                            "line": { "type": "integer", "minimum": 1 },
                            "quote": { "type": "string", "description": "Exact text of that production line." },
                        },
                        "required": ["file", "line", "quote"],
                        "additionalProperties": false,
                    },
                },
                "explanation": { "type": "string" },
            }),
            &["verdict", "code_under_test", "explanation"],
        ),
    ]
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
