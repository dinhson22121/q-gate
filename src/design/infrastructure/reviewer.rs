use std::sync::Arc;

use futures::future::BoxFuture;

use super::claude_states::ClaudeStatesJudge;
use super::i18n_fs::FsI18nSource;
use crate::design::application::{
    DesignCache, DesignCheckKind, DesignOutcome, DesignRequest, FigmaSource, I18nSource, ReviewDesign, StatesJudge,
};
use crate::llm::StructuredModel;
use crate::review::application::agent_ports::{AgentError, AgentInput, DesignReviewer};
use crate::review::application::{DesignMismatch, TicketBrief};

pub struct FigmaDesignReviewer {
    use_case: ReviewDesign,
}

impl FigmaDesignReviewer {
    pub fn new(
        source: Arc<dyn FigmaSource>,
        cache: Arc<dyn DesignCache>,
        llm: Option<Arc<dyn StructuredModel>>,
        checks: Vec<DesignCheckKind>,
    ) -> Self {
        let states = llm.map(|llm| Arc::new(ClaudeStatesJudge::new(llm)) as Arc<dyn StatesJudge>);
        Self::with_ports(source, cache, Arc::new(FsI18nSource), states, checks)
    }

    pub fn with_ports(
        source: Arc<dyn FigmaSource>,
        cache: Arc<dyn DesignCache>,
        i18n: Arc<dyn I18nSource>,
        states: Option<Arc<dyn StatesJudge>>,
        checks: Vec<DesignCheckKind>,
    ) -> Self {
        Self { use_case: ReviewDesign::new(source, cache, i18n, states, checks) }
    }

    pub async fn review_with_notes(&self, input: &AgentInput<'_>, brief: Option<&TicketBrief>) -> DesignOutcome {
        let links = input.ticket.map(|t| t.design_links.as_slice()).unwrap_or_default();
        let request = DesignRequest {
            files: input.files,
            links,
            repo_root: input.repo_root,
            context: brief.map(|b| b.context.as_str()),
        };
        self.use_case.execute(request).await
    }
}

fn into_result(outcome: DesignOutcome) -> Result<Vec<DesignMismatch>, AgentError> {
    if outcome.mismatches.is_empty() && !outcome.notes.is_empty() {
        return Err(AgentError(outcome.notes.join("; ")));
    }
    for note in &outcome.notes {
        eprintln!("qgate design: {note}");
    }
    Ok(outcome.mismatches)
}

impl DesignReviewer for FigmaDesignReviewer {
    fn review<'a>(
        &'a self,
        input: &'a AgentInput<'a>,
        brief: Option<&'a TicketBrief>,
    ) -> BoxFuture<'a, Result<Vec<DesignMismatch>, AgentError>> {
        Box::pin(async move { into_result(self.review_with_notes(input, brief).await) })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::design::application::{DesignError, FetchedDesign, FigmaLink, NO_LINKS_NOTE};
    use crate::design::domain::fixtures::{spec, ui_file};
    use crate::review::application::TicketContext;
    use crate::review::domain::change_set::FileChange;

    struct DownSource;

    impl FigmaSource for DownSource {
        fn file_version<'a>(&'a self, _k: &'a str) -> BoxFuture<'a, Result<String, DesignError>> {
            Box::pin(async { Err(DesignError::Unavailable("HTTP 503".into())) })
        }
        fn fetch<'a>(&'a self, _l: &'a FigmaLink, _p: bool) -> BoxFuture<'a, Result<FetchedDesign, DesignError>> {
            Box::pin(async { Err(DesignError::Unavailable("HTTP 503".into())) })
        }
    }

    struct NoCache;

    impl DesignCache for NoCache {
        fn get<'a>(
            &'a self,
            _l: &'a FigmaLink,
        ) -> BoxFuture<'a, Result<Option<crate::design::application::CachedDesign>, DesignError>> {
            Box::pin(async { Ok(None) })
        }
        fn put<'a>(&'a self, _l: &'a FigmaLink, _d: &'a FetchedDesign) -> BoxFuture<'a, Result<(), DesignError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn reviewer() -> FigmaDesignReviewer {
        FigmaDesignReviewer::new(Arc::new(DownSource), Arc::new(NoCache), None, vec![DesignCheckKind::Copy])
    }

    async fn review(files: &[FileChange], ticket: Option<&TicketContext>) -> Result<Vec<DesignMismatch>, AgentError> {
        let input = AgentInput {
            files,
            requirements: None,
            ticket,
            commit: "abc",
            repo_root: Path::new("/nonexistent"),
            snapshot_root: None,
            callers: &[],
        };
        reviewer().review(&input, None).await
    }

    #[tokio::test]
    async fn no_ui_files_is_an_empty_result() {
        let ticket = TicketContext {
            design_links: vec!["https://www.figma.com/design/K/x?node-id=1-2".into()],
            ..TicketContext::default()
        };
        assert_eq!(review(&[ui_file("main.go", &["x := 1"])], Some(&ticket)).await, Ok(Vec::new()));
    }

    #[tokio::test]
    async fn no_ticket_or_links_is_a_note_friendly_error() {
        let files = [ui_file("src/Pay.tsx", &["<b>x</b>"])];
        assert_eq!(review(&files, None).await, Err(AgentError(NO_LINKS_NOTE.into())));
        let ticket = TicketContext::default();
        assert_eq!(review(&files, Some(&ticket)).await, Err(AgentError(NO_LINKS_NOTE.into())));
    }

    #[tokio::test]
    async fn figma_failure_becomes_a_note_not_a_crash() {
        let files = [ui_file("src/Pay.tsx", &["<b>x</b>"])];
        let ticket = TicketContext {
            design_links: vec!["https://www.figma.com/design/K/x?node-id=1-2".into()],
            ..TicketContext::default()
        };
        let err = review(&files, Some(&ticket)).await.unwrap_err();
        assert!(err.0.contains("skipped: Figma unavailable: HTTP 503"), "{err}");
    }

    #[test]
    fn partial_results_keep_mismatches() {
        let mismatch = DesignMismatch {
            kind: DesignCheckKind::Copy,
            frame: spec(&[]).frame_name,
            node_id: "1:2".into(),
            design_value: "Pay now".into(),
            file: "a.tsx".into(),
            line: 1,
            code_evidence: "x".into(),
            explanation: String::new(),
        };
        let outcome = DesignOutcome { mismatches: vec![mismatch.clone()], notes: vec!["frame 2 failed".into()] };
        assert_eq!(into_result(outcome), Ok(vec![mismatch]));
        assert_eq!(into_result(DesignOutcome::default()), Ok(Vec::new()));
    }
}
