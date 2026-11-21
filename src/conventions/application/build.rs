use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;

use super::ports::{ConventionsError, DraftRequest, RepoReader, RuleModel, RulebookStore};
use crate::conventions::domain::evidence::validate_drafts;
use crate::conventions::domain::rulebook::Rulebook;
use crate::conventions::domain::sampling::{
    CodeCandidate, SAMPLE_FILE_CAP, SAMPLE_TOTAL_CAP, candidate_paths, choose_samples, count_imports, language_counts,
    truncate_text, within_budget,
};
use crate::conventions::domain::sources::{
    RULE_SOURCE_CAP, SourceFile, fingerprint, layout, relevant_content, rule_source_paths,
};

pub fn repo_key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

pub(crate) async fn blocking<T, F>(work: F) -> Result<T, ConventionsError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ConventionsError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ConventionsError::Storage(format!("background worker failed: {err}")))?
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoSnapshot {
    pub paths: Vec<String>,
    pub sources: Vec<SourceFile>,
    pub layout: Vec<String>,
    pub fingerprint: String,
}

pub(crate) fn read_snapshot(reader: &dyn RepoReader, root: &Path) -> Result<RepoSnapshot, ConventionsError> {
    let paths = reader.list_files(root)?;
    let mut sources = Vec::new();
    for path in rule_source_paths(&paths) {
        let Some(raw) = reader.read_capped(root, &path, RULE_SOURCE_CAP)? else { continue };
        if let Some(content) = relevant_content(&path, &raw) {
            sources.push(SourceFile { path, content });
        }
    }
    let layout = layout(&paths);
    let fingerprint = fingerprint(&sources, &layout);
    Ok(RepoSnapshot { paths, sources, layout, fingerprint })
}

fn read_samples(reader: &dyn RepoReader, root: &Path, paths: &[String]) -> Result<Vec<SourceFile>, ConventionsError> {
    let mut contents = HashMap::new();
    let mut candidates = Vec::new();
    for path in candidate_paths(paths) {
        let Some(text) = reader.read_capped(root, &path, SAMPLE_FILE_CAP)? else { continue };
        let content = truncate_text(&text, SAMPLE_FILE_CAP).to_string();
        candidates.push(CodeCandidate { path: path.clone(), import_count: count_imports(&content) });
        contents.insert(path, content);
    }
    let chosen = choose_samples(&candidates)
        .into_iter()
        .filter_map(|path| contents.remove(&path).map(|content| SourceFile { path, content }))
        .map(|file| {
            let size = file.content.len();
            (file, size)
        })
        .collect();
    Ok(within_budget(chosen, SAMPLE_TOTAL_CAP))
}

fn draft_request(reader: &dyn RepoReader, root: &Path) -> Result<(DraftRequest, String), ConventionsError> {
    let snapshot = read_snapshot(reader, root)?;
    let samples = read_samples(reader, root, &snapshot.paths)?;
    let mut languages: Vec<(String, usize)> =
        language_counts(&snapshot.paths).into_iter().map(|(l, n)| (l.to_string(), n)).collect();
    languages.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let request = DraftRequest { sources: snapshot.sources, samples, layout: snapshot.layout, languages };
    Ok((request, snapshot.fingerprint))
}

pub struct BuildRulebook {
    model: Arc<dyn RuleModel>,
    reader: Arc<dyn RepoReader>,
    store: Arc<dyn RulebookStore>,
}

impl BuildRulebook {
    pub fn new(model: Arc<dyn RuleModel>, reader: Arc<dyn RepoReader>, store: Arc<dyn RulebookStore>) -> Self {
        Self { model, reader, store }
    }

    pub async fn current_fingerprint(&self, root: &Path) -> Result<String, ConventionsError> {
        let reader = self.reader.clone();
        let root = root.to_path_buf();
        blocking(move || read_snapshot(reader.as_ref(), &root).map(|s| s.fingerprint)).await
    }

    pub async fn execute(&self, root: &Path) -> Result<Rulebook, ConventionsError> {
        let reader = self.reader.clone();
        let owned_root = root.to_path_buf();
        let (request, fingerprint) = blocking(move || draft_request(reader.as_ref(), &owned_root)).await?;
        let drafts = self.model.draft_rules(&request).await?;
        let shown: HashMap<String, String> =
            request.sources.iter().chain(&request.samples).map(|f| (f.path.clone(), f.content.clone())).collect();
        let validated = validate_drafts(drafts, &shown);
        let repo = repo_key(root);
        let store = self.store.clone();
        blocking(move || {
            let decisions = store.decisions(&repo)?;
            let mut drafts = store.load(&repo)?.map(|existing| existing.team_drafts()).unwrap_or_default();
            drafts.extend(validated);
            let rulebook = Rulebook::assemble(repo, fingerprint, drafts, &decisions, Utc::now());
            store.save(&rulebook)?;
            Ok(rulebook)
        })
        .await
    }
}
