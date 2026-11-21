use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use serde::Serialize;

use super::{ReviewFileSink, StoredFile};
use crate::handoff::domain::path_template::{LATEST_STEM, join};
use crate::handoff::domain::{
    HandoffDocument, HandoffInput, PathTemplate, PathValues, RuleBook, RulesPaths, Sighting, StemMatcher,
    TemplateError, render_html, render_json, render_markdown, render_rules_markdown,
};
use crate::settings::application::{OutputConfig, OutputFormat};

const MD: &str = "md";
const JSON: &str = "json";
const HTML: &str = "html";
const MAX_RULE_STATE_BYTES: u64 = 4 * 1024 * 1024;

static RULES_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Disabled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExportOutcome {
    pub written: Vec<String>,
    pub deleted: Vec<String>,
    pub skipped: Option<SkipReason>,
    pub errors: Vec<String>,
    pub rules_file: Option<String>,
}

impl ExportOutcome {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn markdown_path(&self) -> Option<&str> {
        self.written.iter().map(String::as_str).find(|p| p.ends_with(".md") && Some(*p) != self.rules_file.as_deref())
    }
}

pub struct ExportReview {
    sink: Arc<dyn ReviewFileSink>,
    config: OutputConfig,
    template: PathTemplate,
}

impl ExportReview {
    pub fn new(sink: Arc<dyn ReviewFileSink>, config: &OutputConfig) -> Result<Self, TemplateError> {
        let template = PathTemplate::parse(&config.path_template)?;
        Ok(Self { sink, config: config.clone(), template })
    }

    pub fn execute(&self, input: &HandoffInput<'_>) -> ExportOutcome {
        let mut outcome = ExportOutcome::default();
        if !self.config.enabled {
            outcome.skipped = Some(SkipReason::Disabled);
            return outcome;
        }
        let mut doc = HandoffDocument::build(input);
        let meta = &input.meta;
        let values = PathValues::new(
            &meta.repo,
            meta.branch.as_deref(),
            &meta.commit,
            doc.primary_ticket(),
            meta.reviewed_at,
            doc.verdict.passed,
        );
        if self.config.rules_enabled {
            self.learn_rules(&mut outcome, &mut doc, &values, input);
        }
        let files = self.render(&doc, input);
        let path = self.template.render(&values);
        let per_commit = self.config.include_passing || !doc.verdict.passed;
        if per_commit {
            for (extension, bytes) in &files {
                self.write(&mut outcome, &path.file(extension), bytes);
            }
        }
        if self.config.write_latest {
            self.write_latest(&mut outcome, &values.latest_dir(), &files);
        }
        if per_commit
            && self.config.keep_last > 0
            && let Some(matcher) = self.template.stem_matcher(&values)
        {
            self.prune(&mut outcome, &path.dir, &path.stem, &matcher);
        }
        outcome
    }

    fn learn_rules(
        &self,
        outcome: &mut ExportOutcome,
        doc: &mut HandoffDocument,
        values: &PathValues,
        input: &HandoffInput<'_>,
    ) {
        if doc.rules_to_add.is_empty() {
            return;
        }
        let paths = RulesPaths::new(&values.repo, &self.config.rules_file);
        let _guard = RULES_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let book = match self.sink.read(&paths.state, MAX_RULE_STATE_BYTES) {
            Ok(None) => RuleBook::default(),
            Ok(Some(bytes)) => match RuleBook::from_json(&bytes) {
                Ok(book) => book,
                Err(e) => {
                    outcome.errors.push(format!("cannot use {}: {e}; fix or delete it to start over", paths.state));
                    return;
                }
            },
            Err(e) => {
                outcome.errors.push(format!("cannot read {}: {e}", paths.state));
                return;
            }
        };
        let seen = Sighting { commit: values.sha7.clone(), at: input.meta.reviewed_at };
        let merged = book.merge(&doc.rules_to_add, &seen, self.config.rules_max as usize);
        let before = outcome.errors.len();
        self.write(outcome, &paths.state, merged.book.to_json().as_bytes());
        if outcome.errors.len() > before {
            return;
        }
        let markdown = render_rules_markdown(&merged.book, &input.meta.repo, &paths);
        self.write(outcome, &paths.markdown, markdown.as_bytes());
        if outcome.errors.len() == before {
            outcome.rules_file = Some(paths.markdown.clone());
            doc.rules_file = Some(paths.markdown);
        }
        doc.rules_to_add = merged.added;
    }

    fn render(&self, doc: &HandoffDocument, input: &HandoffInput<'_>) -> Vec<(&'static str, Vec<u8>)> {
        let mut files = Vec::new();
        if self.config.formats.contains(&OutputFormat::Markdown) {
            files.push((MD, render_markdown(doc).into_bytes()));
        }
        if self.config.formats.contains(&OutputFormat::Json) {
            files.push((JSON, render_json(doc, input).into_bytes()));
        }
        if self.config.formats.contains(&OutputFormat::Html) {
            files.push((HTML, render_html(doc, input).into_bytes()));
        }
        files
    }

    fn write(&self, outcome: &mut ExportOutcome, rel_path: &str, bytes: &[u8]) {
        match self.sink.write(rel_path, bytes) {
            Ok(()) => outcome.written.push(rel_path.to_string()),
            Err(e) => outcome.errors.push(format!("cannot write {rel_path}: {e}")),
        }
    }

    fn write_latest(&self, outcome: &mut ExportOutcome, dir: &str, files: &[(&'static str, Vec<u8>)]) {
        for (extension, bytes) in files {
            self.write(outcome, &join(dir, &format!("{LATEST_STEM}.{extension}")), bytes);
        }
        for extension in [MD, JSON, HTML].into_iter().filter(|e| !files.iter().any(|(x, _)| x == e)) {
            let stale = join(dir, &format!("{LATEST_STEM}.{extension}"));
            if let Err(e) = self.sink.delete(&stale) {
                outcome.errors.push(format!("cannot delete stale {stale}: {e}"));
            }
        }
    }

    fn prune(&self, outcome: &mut ExportOutcome, dir: &str, current: &str, matcher: &StemMatcher) {
        let listed = match self.sink.list(dir) {
            Ok(files) => files,
            Err(e) => {
                outcome.errors.push(format!("cannot list {dir} for pruning: {e}"));
                return;
            }
        };
        let mut by_format: BTreeMap<&str, BTreeMap<&str, (SystemTime, Vec<&str>)>> = BTreeMap::new();
        let rules = RulesPaths::new("", &self.config.rules_file);
        for StoredFile { name, modified } in listed.iter().filter(|f| !rules.file_names().contains(&f.name.as_str())) {
            if let (Some(stem), Some((_, extension))) = (matcher.stem_of(name), name.rsplit_once('.')) {
                let run =
                    by_format.entry(extension).or_default().entry(stem).or_insert((SystemTime::UNIX_EPOCH, Vec::new()));
                run.0 = run.0.max(*modified);
                run.1.push(name);
            }
        }
        for runs in by_format.into_values() {
            let mut ordered: Vec<(&str, SystemTime, Vec<&str>)> =
                runs.into_iter().map(|(stem, (modified, names))| (stem, modified, names)).collect();
            ordered.sort_by_key(|(stem, modified, _)| (*stem != current, Reverse(*modified), Reverse(*stem)));
            let keep = self.config.keep_last as usize - usize::from(ordered.iter().all(|(stem, ..)| *stem != current));
            for (_, _, names) in ordered.into_iter().skip(keep) {
                self.delete_all(outcome, dir, &names);
            }
        }
    }

    fn delete_all(&self, outcome: &mut ExportOutcome, dir: &str, names: &[&str]) {
        for name in names {
            let rel = join(dir, name);
            match self.sink.delete(&rel) {
                Ok(()) => outcome.deleted.push(rel),
                Err(e) => outcome.errors.push(format!("cannot delete {rel}: {e}")),
            }
        }
    }
}
