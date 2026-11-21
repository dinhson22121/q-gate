use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::design::domain::ui::{changed_ui_files, is_style_file, reviewable};
use crate::design::domain::{DesignCheckKind, DesignSpec, FigmaLink, detect_mismatches};
use crate::review::application::DesignMismatch;
use crate::review::domain::change_set::FileChange;

use super::ports::{DesignCache, DesignError, FetchedDesign, FigmaSource, I18nSource, StatesJudge, StatesRequest};

pub const MAX_LINKS: usize = 5;
pub const NO_LINKS_NOTE: &str = "design check skipped: no Figma frame link on the ticket or its parent Story";

pub struct DesignRequest<'a> {
    pub files: &'a [FileChange],
    pub links: &'a [String],
    pub repo_root: &'a Path,
    pub context: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DesignOutcome {
    pub mismatches: Vec<DesignMismatch>,
    pub notes: Vec<String>,
}

pub struct ReviewDesign {
    source: Arc<dyn FigmaSource>,
    cache: Arc<dyn DesignCache>,
    i18n: Arc<dyn I18nSource>,
    states: Option<Arc<dyn StatesJudge>>,
    checks: Vec<DesignCheckKind>,
}

struct Frame {
    spec: DesignSpec,
    png: Option<Vec<u8>>,
}

impl ReviewDesign {
    pub fn new(
        source: Arc<dyn FigmaSource>,
        cache: Arc<dyn DesignCache>,
        i18n: Arc<dyn I18nSource>,
        states: Option<Arc<dyn StatesJudge>>,
        checks: Vec<DesignCheckKind>,
    ) -> Self {
        Self { source, cache, i18n, states, checks }
    }

    fn wants(&self, check: DesignCheckKind) -> bool {
        self.checks.contains(&check)
    }

    fn runs_states(&self) -> bool {
        self.wants(DesignCheckKind::States) && self.states.is_some()
    }

    pub async fn execute(&self, request: DesignRequest<'_>) -> DesignOutcome {
        let mut outcome = DesignOutcome::default();
        let ui_files = changed_ui_files(request.files);
        let has_styles = request.files.iter().any(|f| reviewable(f) && is_style_file(&f.path));
        if self.checks.is_empty() || (ui_files.is_empty() && !has_styles) {
            return outcome;
        }
        if self.wants(DesignCheckKind::States) && self.states.is_none() {
            outcome.notes.push("design states check skipped: no Claude client configured".into());
        }
        let links = parse_links(request.links, &mut outcome.notes);
        if links.is_empty() {
            if request.links.is_empty() {
                outcome.notes.push(NO_LINKS_NOTE.into());
            }
            return outcome;
        }
        let i18n = if self.wants(DesignCheckKind::Copy) { self.i18n.load(request.repo_root).await } else { Vec::new() };
        let mut versions: HashMap<String, Result<String, DesignError>> = HashMap::new();
        for link in &links {
            let Some(frame) = self.frame(link, &mut versions, &mut outcome.notes).await else { continue };
            outcome.mismatches.extend(detect_mismatches(&frame.spec, request.files, &i18n, &self.checks));
            if !ui_files.is_empty() {
                self.judge_states(&frame, &ui_files, request.context, &mut outcome).await;
            }
        }
        outcome.mismatches = deduplicate(std::mem::take(&mut outcome.mismatches));
        outcome
    }

    async fn judge_states(
        &self,
        frame: &Frame,
        ui_files: &[&FileChange],
        context: Option<&str>,
        outcome: &mut DesignOutcome,
    ) {
        let Some(judge) = self.states.as_ref().filter(|_| self.wants(DesignCheckKind::States)) else { return };
        if frame.spec.variants.is_empty() && frame.png.is_none() {
            return;
        }
        let request = StatesRequest { spec: &frame.spec, png: frame.png.as_deref(), files: ui_files, context };
        match judge.judge(request).await {
            Ok(found) => outcome.mismatches.extend(found),
            Err(error) => outcome.notes.push(format!("frame {}: {error}", frame.spec.node_id)),
        }
    }

    async fn frame(
        &self,
        link: &FigmaLink,
        versions: &mut HashMap<String, Result<String, DesignError>>,
        notes: &mut Vec<String>,
    ) -> Option<Frame> {
        let cached = self.cache.get(link).await.unwrap_or_else(|error| {
            notes.push(format!("Figma frame {link}: {error}"));
            None
        });
        if !versions.contains_key(&link.file_key) {
            let version = self.source.file_version(&link.file_key).await;
            versions.insert(link.file_key.clone(), version);
        }
        let need_png = self.runs_states();
        match (versions.get(&link.file_key), cached) {
            (Some(Ok(version)), Some(hit)) if hit.version == *version && (!need_png || hit.png.is_some()) => {
                Some(Frame { spec: hit.spec, png: hit.png })
            }
            (Some(Err(error)), Some(hit)) => {
                notes.push(format!("Figma frame {link}: {error}; using the cached version {}", hit.version));
                Some(Frame { spec: hit.spec, png: hit.png })
            }
            (Some(Err(error)), None) => {
                notes.push(format!("Figma frame {link} skipped: {error}"));
                None
            }
            _ => self.fetch(link, need_png, notes).await,
        }
    }

    async fn fetch(&self, link: &FigmaLink, with_png: bool, notes: &mut Vec<String>) -> Option<Frame> {
        let fetched: FetchedDesign = match self.source.fetch(link, with_png).await {
            Ok(fetched) => fetched,
            Err(error) => {
                notes.push(format!("Figma frame {link} skipped: {error}"));
                return None;
            }
        };
        notes.extend(fetched.notes.iter().map(|n| format!("Figma frame {link}: {n}")));
        if let Err(error) = self.cache.put(link, &fetched).await {
            notes.push(format!("Figma frame {link}: {error}"));
        }
        Some(Frame { spec: fetched.spec, png: fetched.png })
    }
}

fn parse_links(raw: &[String], notes: &mut Vec<String>) -> Vec<FigmaLink> {
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for link in raw {
        match FigmaLink::parse(link) {
            Ok(parsed) if seen.insert(parsed.clone()) => links.push(parsed),
            Ok(_) => {}
            Err(error) => notes.push(format!("ignored design link: {error}")),
        }
    }
    if links.len() > MAX_LINKS {
        notes.push(format!("only the first {MAX_LINKS} of {} Figma frames were checked", links.len()));
        links.truncate(MAX_LINKS);
    }
    links
}

fn deduplicate(mut mismatches: Vec<DesignMismatch>) -> Vec<DesignMismatch> {
    let mut seen = HashSet::new();
    mismatches.retain(|m| seen.insert((m.kind, m.file.clone(), m.line, m.design_value.clone())));
    mismatches
}

#[cfg(test)]
#[path = "review_design_tests.rs"]
mod tests;
