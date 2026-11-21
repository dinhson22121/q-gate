use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures::future::BoxFuture;

use super::ports::ChangeSource;
use crate::review::domain::change_set::ChangeSet;
use crate::review::domain::report::ReviewReport;
use crate::review::domain::requirements::Requirements;

const CALL_OVERHEAD_CHARS: usize = 14_000;
const TEST_SESSION_CHARS: usize = 60_000;

const AUTH_HINTS: [&str; 5] = ["not logged in", "/login", "authentication", "oauth", "cannot start"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    RateLimited { resets_at: Option<String> },
    Failed(String),
}

pub trait ModelPreflight: Send + Sync {
    fn probe(&self) -> BoxFuture<'_, Result<(), ProbeFailure>>;
    fn usage_limit_marker(&self) -> &str;
    fn tokens_for_chars(&self, chars: usize) -> u64;
}

#[derive(Clone, Copy)]
pub struct ReviewScope {
    pub changed_bytes: usize,
    pub guideline_bytes: usize,
    pub ticket_bytes: usize,
    pub has_criteria: bool,
    pub scenarios: usize,
    pub tests_enabled: bool,
    pub architecture_enabled: bool,
}

#[derive(Clone, Copy)]
pub struct AgentPlan {
    pub agents_enabled: bool,
    pub scenarios: usize,
    pub tests_enabled: bool,
    pub architecture_enabled: bool,
}

pub struct ReviewPreflight {
    model: Arc<dyn ModelPreflight>,
}

impl ReviewPreflight {
    pub fn new(model: Arc<dyn ModelPreflight>) -> Self {
        Self { model }
    }

    pub async fn ensure_available(&self) -> Result<()> {
        match self.model.probe().await {
            Ok(()) => Ok(()),
            Err(ProbeFailure::RateLimited { resets_at }) => Err(anyhow!(
                "qgate: not running - {}, waiting for session reset{}",
                self.model.usage_limit_marker(),
                resets_at.map(|t| format!(" (resets {t})")).unwrap_or_default()
            )),
            Err(ProbeFailure::Failed(error)) if looks_like_auth_problem(&error) => Err(anyhow!(
                "qgate: not running - Claude CLI is not usable: {error}. Log in (`claude login`) or set \
                 CLAUDE_CODE_OAUTH_TOKEN (from `claude setup-token`); see the README, \"Claude authentication\""
            )),
            Err(ProbeFailure::Failed(error)) => {
                eprintln!("qgate: warning: Claude preflight failed, continuing: {error}");
                Ok(())
            }
        }
    }

    pub fn estimate_review_tokens(&self, scope: &ReviewScope) -> u64 {
        let diff = scope.changed_bytes;
        let mut chars = CALL_OVERHEAD_CHARS + scope.guideline_bytes + diff;
        if scope.has_criteria {
            chars += 2 * CALL_OVERHEAD_CHARS + 2 * scope.ticket_bytes + diff;
            if scope.tests_enabled {
                chars += scope.scenarios * TEST_SESSION_CHARS;
            }
        }
        if scope.architecture_enabled {
            chars += CALL_OVERHEAD_CHARS + diff;
        }
        self.model.tokens_for_chars(chars)
    }

    pub async fn log_estimate(
        &self,
        changes: &impl ChangeSource,
        plan: AgentPlan,
        requirements: Option<&Requirements>,
        guidelines: &str,
    ) {
        let Ok(changes) = changes.load().await else { return };
        let criteria = requirements.map_or(&[][..], |r| r.criteria.as_slice());
        let scope = ReviewScope {
            changed_bytes: changed_bytes(&changes),
            guideline_bytes: guidelines.len(),
            ticket_bytes: criteria.iter().map(|c| c.text.len()).sum(),
            has_criteria: plan.agents_enabled && !criteria.is_empty(),
            scenarios: plan.scenarios,
            tests_enabled: plan.agents_enabled && plan.tests_enabled,
            architecture_enabled: plan.agents_enabled && plan.architecture_enabled,
        };
        eprintln!("qgate: estimated ~{} input tokens for this review", self.estimate_review_tokens(&scope));
    }

    pub fn usage_limit_note<'a>(&self, notes: &'a [String]) -> Option<&'a str> {
        let marker = self.model.usage_limit_marker();
        notes.iter().map(String::as_str).find(|note| note.contains(marker))
    }

    pub fn settle_agent_spend(&self, report: &mut ReviewReport, spent_usd: f64, run_notes: &[String]) -> Result<()> {
        let Some(agent_report) = report.agents.as_mut() else { return Ok(()) };
        if let Some(note) = self.usage_limit_note(&agent_report.notes) {
            return Err(anyhow!("qgate: review aborted, an agent hit the Claude usage limit mid-review ({note})"));
        }
        agent_report.cost_usd = Some(spent_usd);
        agent_report.notes.extend(run_notes.iter().cloned());
        report.estimated_cost_usd = Some(report.estimated_cost_usd.unwrap_or(0.0) + spent_usd);
        Ok(())
    }
}

fn changed_bytes(changes: &ChangeSet) -> usize {
    changes.files.iter().map(|f| f.review_weight()).sum()
}

fn looks_like_auth_problem(message: &str) -> bool {
    let lower = message.to_lowercase();
    AUTH_HINTS.iter().any(|hint| lower.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::agent_results::AgentReport;

    const MARKER: &str = "Claude usage limit reached";

    struct Probe(Result<(), ProbeFailure>);

    impl ModelPreflight for Probe {
        fn probe(&self) -> BoxFuture<'_, Result<(), ProbeFailure>> {
            let result = self.0.clone();
            Box::pin(async move { result })
        }

        fn usage_limit_marker(&self) -> &str {
            MARKER
        }

        fn tokens_for_chars(&self, chars: usize) -> u64 {
            chars.div_ceil(4) as u64
        }
    }

    fn preflight(result: Result<(), ProbeFailure>) -> ReviewPreflight {
        ReviewPreflight::new(Arc::new(Probe(result)))
    }

    async fn outcome(result: Result<(), ProbeFailure>) -> Result<()> {
        preflight(result).ensure_available().await
    }

    #[tokio::test]
    async fn rate_limit_aborts_with_the_reset_time() {
        let limited = ProbeFailure::RateLimited { resets_at: Some("4:30pm".into()) };
        let message = outcome(Err(limited)).await.unwrap_err().to_string();
        assert!(message.contains("not running") && message.contains("waiting for session reset (resets 4:30pm)"));
        let no_time = ProbeFailure::RateLimited { resets_at: None };
        assert!(!outcome(Err(no_time)).await.unwrap_err().to_string().contains("resets"));
    }

    #[tokio::test]
    async fn missing_auth_or_binary_aborts_with_login_instructions() {
        for text in ["Not logged in - Please run /login", "cannot start claude: No such file"] {
            let message = outcome(Err(ProbeFailure::Failed(text.into()))).await.unwrap_err().to_string();
            assert!(message.contains("CLAUDE_CODE_OAUTH_TOKEN"), "{message}");
        }
    }

    #[tokio::test]
    async fn other_probe_failures_only_warn() {
        assert!(outcome(Ok(())).await.is_ok());
        assert!(outcome(Err(ProbeFailure::Failed("did not finish in 600s".into()))).await.is_ok());
    }

    #[test]
    fn estimate_grows_with_each_enabled_agent() {
        let base = ReviewScope {
            changed_bytes: 40_000,
            guideline_bytes: 4_000,
            ticket_bytes: 2_000,
            has_criteria: false,
            scenarios: 3,
            tests_enabled: true,
            architecture_enabled: false,
        };
        let subject = preflight(Ok(()));
        let plain = subject.estimate_review_tokens(&base);
        assert_eq!(plain, subject.model.tokens_for_chars(CALL_OVERHEAD_CHARS + 44_000));
        let with_arch = subject.estimate_review_tokens(&ReviewScope { architecture_enabled: true, ..base });
        assert!(with_arch > plain);
        let with_ticket = subject.estimate_review_tokens(&ReviewScope { has_criteria: true, ..base });
        assert!(with_ticket > plain + subject.model.tokens_for_chars(3 * TEST_SESSION_CHARS) - 1);
    }

    fn report_with_agent_notes(notes: Vec<String>, estimated_cost_usd: Option<f64>) -> ReviewReport {
        ReviewReport {
            agents: Some(AgentReport { notes, ..AgentReport::default() }),
            estimated_cost_usd,
            ..ReviewReport::default()
        }
    }

    #[test]
    fn agent_spend_is_added_to_the_agent_report_and_the_review_cost() {
        let mut report = report_with_agent_notes(vec!["from agents".into()], Some(0.5));

        preflight(Ok(())).settle_agent_spend(&mut report, 1.25, &["design review disabled".to_string()]).unwrap();

        let agents = report.agents.unwrap();
        assert_eq!(agents.cost_usd, Some(1.25));
        assert_eq!(agents.notes, ["from agents", "design review disabled"]);
        assert_eq!(report.estimated_cost_usd, Some(1.75));
    }

    #[test]
    fn agent_spend_starts_the_review_cost_when_there_was_none() {
        let mut report = report_with_agent_notes(Vec::new(), None);
        preflight(Ok(())).settle_agent_spend(&mut report, 0.4, &[]).unwrap();
        assert_eq!(report.estimated_cost_usd, Some(0.4));
    }

    #[test]
    fn a_usage_limit_note_aborts_the_review_before_any_cost_is_recorded() {
        let mut report = report_with_agent_notes(vec![format!("test agents failed: {MARKER} (resets 5pm)")], None);

        let error = preflight(Ok(())).settle_agent_spend(&mut report, 1.0, &[]).unwrap_err().to_string();

        assert!(error.contains("usage limit mid-review") && error.contains("resets 5pm"), "{error}");
        assert_eq!(report.estimated_cost_usd, None);
    }

    #[test]
    fn a_report_without_agents_is_left_untouched() {
        let mut report = ReviewReport::default();
        preflight(Ok(())).settle_agent_spend(&mut report, 1.0, &[]).unwrap();
        assert_eq!(report, ReviewReport::default());
    }

    #[test]
    fn usage_limit_notes_are_found() {
        let notes =
            vec!["ticket analysis failed: boom".to_string(), format!("test agents failed: {MARKER} (resets 5pm)")];
        let subject = preflight(Ok(()));
        assert!(subject.usage_limit_note(&notes).unwrap().contains("resets 5pm"));
        assert!(subject.usage_limit_note(&notes[..1]).is_none());
    }
}
