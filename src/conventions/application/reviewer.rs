//! `RulebookReviewer`: checks one change against the repository's current rulebook.

use std::sync::Arc;

use futures::future::BoxFuture;

use super::changes::changed_files;
use super::ports::{CheckRequest, ConventionsError, ViolationDraft};
use super::service::RulebookService;
use crate::conventions::domain::evidence::{ChangedFile, quote_in_added_code};
use crate::conventions::domain::rulebook::{Rule, RuleStatus, Rulebook};
use crate::conventions::domain::scope::select_rules;
use crate::review::application::RuleViolation;
use crate::review::application::TicketBrief;
use crate::review::application::agent_ports::{AgentError, AgentInput, ArchitectureReviewer};

pub struct RulebookReviewer {
    service: Arc<RulebookService>,
    use_inferred_rules: bool,
    refresh_days: u32,
}

impl RulebookReviewer {
    /// `use_inferred_rules`: also check rules nobody has approved yet (reported with `approved = false`).
    /// `refresh_days`: rebuild a rulebook older than this even if no rule source changed.
    pub fn with_service(service: Arc<RulebookService>, use_inferred_rules: bool, refresh_days: u32) -> Self {
        Self { service, use_inferred_rules, refresh_days }
    }

    async fn check(
        &self,
        input: &AgentInput<'_>,
        brief: Option<&TicketBrief>,
    ) -> Result<Vec<RuleViolation>, ConventionsError> {
        let files = changed_files(input.files);
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let rulebook = self.service.current(input.repo_root, self.refresh_days).await?;
        let usable: Vec<Rule> =
            rulebook.rules.iter().filter(|r| r.is_usable(self.use_inferred_rules)).cloned().collect();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        let selected: Vec<&str> =
            select_rules(&usable, &paths, self.use_inferred_rules).iter().map(|r| r.id.as_str()).collect();
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        let context = brief.map(|b| b.context.as_str()).filter(|c| !c.trim().is_empty());
        let request = CheckRequest { rules: &usable, selected: &selected, files: &files, context };
        let drafts = self.service.model().find_violations(request).await?;
        Ok(ground_violations(&rulebook, &selected, &files, drafts))
    }
}

/// Keeps violations of selected rules whose evidence is verbatim new code; fills title and approval.
fn ground_violations(
    rulebook: &Rulebook,
    selected: &[&str],
    files: &[ChangedFile],
    drafts: Vec<ViolationDraft>,
) -> Vec<RuleViolation> {
    drafts
        .into_iter()
        .filter_map(|draft| {
            let rule = rulebook.rule(draft.rule_id.trim()).filter(|r| selected.contains(&r.id.as_str()))?;
            let path = draft.file.trim().trim_start_matches("./");
            let file = files.iter().find(|f| f.path == path)?;
            if !quote_in_added_code(file, draft.line, &draft.evidence) {
                return None;
            }
            Some(RuleViolation {
                rule_id: rule.id.clone(),
                rule_title: rule.title.clone(),
                approved: rule.status == RuleStatus::Approved,
                file: file.path.clone(),
                line: draft.line,
                evidence: draft.evidence,
                explanation: draft.explanation,
                suggestion: draft.suggestion.filter(|s| !s.trim().is_empty()),
            })
        })
        .collect()
}

impl ArchitectureReviewer for RulebookReviewer {
    fn review<'a>(
        &'a self,
        input: &'a AgentInput<'a>,
        brief: Option<&'a TicketBrief>,
    ) -> BoxFuture<'a, Result<Vec<RuleViolation>, AgentError>> {
        Box::pin(async move { self.check(input, brief).await.map_err(|e| AgentError(e.to_string())) })
    }
}
