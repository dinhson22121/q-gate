use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::OnceCell;

use crate::llm::{ContentPart, LoopLimits, LoopStop, SystemBlock, ToolHandler};
use crate::review::application::TestResult;
use crate::review::application::agent_ports::{AgentInput, TestAgent};
use crate::review::application::{TestScenario, UnverifiableReason};
use crate::testing::domain::language::{TestLanguage, select_language};
use crate::testing::domain::profile::{ScenarioName, TestTarget, test_targets};
use crate::testing::domain::race::{RACE_DETECTOR_UNAVAILABLE_NOTE, with_race_detector};
use crate::testing::domain::verdict::{ScenarioOutcome, criterion_texts, decide, inconclusive};

use super::handler::{HandlerLimits, ScenarioToolHandler, tool_specs};
use super::ports::{RaceDetectorProbe, ScenarioSnapshots, TestRunner, ToolLoop, Workspace};
use super::prompt::{SYSTEM_PROMPT, user_prompt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestAgentSettings {
    pub languages: Vec<TestLanguage>,
    pub max_repair_rounds: u32,
    pub timeout: Duration,
    pub max_turns: u32,
}

pub struct ScenarioTestAgent<L, R, W> {
    driver: Arc<L>,
    runner: R,
    snapshots: Arc<dyn ScenarioSnapshots>,
    settings: TestAgentSettings,
    race_probe: Option<Arc<dyn RaceDetectorProbe>>,
    race_detector: OnceCell<bool>,
    workspace: PhantomData<fn() -> W>,
}

impl<L: ToolLoop, R: TestRunner, W: Workspace> ScenarioTestAgent<L, R, W> {
    pub fn with_ports(
        driver: Arc<L>,
        runner: R,
        snapshots: Arc<dyn ScenarioSnapshots>,
        settings: TestAgentSettings,
    ) -> Self {
        Self {
            driver,
            runner,
            snapshots,
            settings,
            race_probe: None,
            race_detector: OnceCell::new(),
            workspace: PhantomData,
        }
    }

    pub fn with_race_probe(mut self, probe: Arc<dyn RaceDetectorProbe>) -> Self {
        self.race_probe = Some(probe);
        self
    }

    async fn race_detector_usable(&self, workspace: &W, targets: &[TestTarget]) -> Option<bool> {
        let probe = self.race_probe.as_ref()?;
        let cwd = workspace.root().join(&targets.first()?.module_dir);
        Some(*self.race_detector.get_or_init(|| probe.available(&cwd)).await)
    }

    pub async fn run_scenario(&self, input: &AgentInput<'_>, scenario: &TestScenario) -> TestResult {
        let id = scenario.id.as_str();
        if input.snapshot_root.is_none() {
            return inconclusive(
                id,
                "",
                UnverifiableReason::TestNotRunnable,
                "no writable snapshot of the commit; tests are never run in the repository",
            );
        }
        let changed: Vec<String> = input.files.iter().map(|f| f.path.clone()).collect();
        let choice = match select_language(&changed, &self.settings.languages) {
            Ok(choice) => choice,
            Err(e) => return inconclusive(id, "", UnverifiableReason::TestNotRunnable, e.to_string()),
        };
        let language = choice.language;
        let snapshot = match self.snapshots.create(input.repo_root, input.commit).await {
            Ok(snapshot) => snapshot,
            Err(e) => {
                let note = format!("cannot create the scenario's snapshot: {e}");
                return inconclusive(id, language.as_str(), UnverifiableReason::ToolError, note);
            }
        };
        let workspace = match W::open(snapshot.path()) {
            Ok(workspace) => workspace,
            Err(e) => {
                let note = format!("cannot open the snapshot: {e}");
                return inconclusive(id, language.as_str(), UnverifiableReason::ToolError, note);
            }
        };
        let files: Vec<String> = choice.files.into_iter().filter(|f| workspace.has_path(f)).collect();
        let mut targets = test_targets(language, &files, &ScenarioName::from_id(id), &workspace);
        let mut remarks = Vec::new();
        if language == TestLanguage::Go {
            match self.race_detector_usable(&workspace, &targets).await {
                Some(true) => targets = targets.into_iter().map(with_race_detector).collect(),
                Some(false) => remarks.push(RACE_DETECTOR_UNAVAILABLE_NOTE.to_string()),
                None => {}
            }
        }
        if targets.is_empty() {
            let note = format!("no {language} module (build file) found for the changed {language} files");
            return inconclusive(id, language.as_str(), UnverifiableReason::TestNotRunnable, note);
        }
        let texts = criterion_texts(input.requirements, &scenario.criterion_ids);
        let limits = HandlerLimits {
            language,
            timeout: self.settings.timeout,
            max_repair_rounds: self.settings.max_repair_rounds,
        };
        let handler = ScenarioToolHandler::new(&workspace, &self.runner, &targets, limits);
        let system = [SystemBlock { text: SYSTEM_PROMPT.to_string(), cache: true }];
        let user = user_prompt(scenario, &texts, language, &files, &targets, self.settings.max_repair_rounds);
        let looped = self
            .driver
            .drive(
                &system,
                vec![ContentPart::Text(user)],
                &tool_specs(),
                &handler,
                LoopLimits { max_turns: self.settings.max_turns },
            )
            .await;
        match looped {
            Err(e) => remarks.push(format!("agent loop failed: {e}")),
            Ok(outcome) if outcome.stop != LoopStop::Finished && !handler.finished() => {
                remarks.push(format!("agent stopped without report_result after {} turns", outcome.turns));
            }
            Ok(_) => {}
        }
        let facts = handler.into_facts();
        let (advisory, code_under_test, explanation) = match facts.report {
            Some(report) => {
                if !report.rejected.is_empty() {
                    remarks.push(format!("unverifiable code references dropped: {}", report.rejected.join("; ")));
                }
                (Some(report.advisory), report.code_under_test, report.explanation)
            }
            None => (None, Vec::new(), String::new()),
        };
        let repo_root = input.repo_root.display().to_string();
        decide(ScenarioOutcome {
            scenario,
            language,
            criterion_texts: texts,
            commit: input.commit,
            repo_root: &repo_root,
            target: facts.target.as_ref(),
            runs: &facts.runs,
            max_repair_rounds: self.settings.max_repair_rounds,
            code_under_test,
            advisory,
            explanation,
            remarks,
        })
    }
}

impl<L: ToolLoop, R: TestRunner, W: Workspace + 'static> TestAgent for ScenarioTestAgent<L, R, W> {
    fn run<'a>(&'a self, input: &'a AgentInput<'a>, scenario: &'a TestScenario) -> BoxFuture<'a, TestResult> {
        Box::pin(self.run_scenario(input, scenario))
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
