use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::*;
use crate::handoff::domain::Placeholder;
use crate::handoff::domain::fixtures::{failing_report, meta, passing_report};
use crate::handoff::infrastructure::FsReviewFileSink;
use crate::review::application::ReviewReport;
use crate::settings::application::{OUTPUT_PLACEHOLDERS, OutputConfig, OutputFormat};

/// In-memory sink; `fail_writes` makes every write fail.
#[derive(Default)]
struct MemorySink {
    files: Mutex<BTreeMap<String, (Vec<u8>, SystemTime)>>,
    fail_writes: bool,
    clock: Mutex<u64>,
}

impl MemorySink {
    fn put(&self, path: &str, age_secs: u64) {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs);
        self.files.lock().unwrap().insert(path.into(), (b"old".to_vec(), at));
    }

    fn paths(&self) -> Vec<String> {
        self.files.lock().unwrap().keys().cloned().collect()
    }

    fn text(&self, path: &str) -> String {
        String::from_utf8(self.files.lock().unwrap()[path].0.clone()).unwrap()
    }
}

impl ReviewFileSink for MemorySink {
    fn write(&self, rel_path: &str, bytes: &[u8]) -> Result<(), SinkError> {
        if self.fail_writes {
            return Err(SinkError("disk full".into()));
        }
        let mut clock = self.clock.lock().unwrap();
        *clock += 1;
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000 + *clock);
        self.files.lock().unwrap().insert(rel_path.into(), (bytes.to_vec(), at));
        Ok(())
    }

    fn read(&self, rel_path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, SinkError> {
        let bytes = self.files.lock().unwrap().get(rel_path).map(|(bytes, _)| bytes.clone());
        match bytes {
            Some(b) if b.len() as u64 > max_bytes => Err(SinkError("too large".into())),
            other => Ok(other),
        }
    }

    fn list(&self, dir_prefix: &str) -> Result<Vec<StoredFile>, SinkError> {
        let prefix = format!("{dir_prefix}/");
        Ok(self
            .files
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(path, (_, modified))| {
                let name = path.strip_prefix(&prefix)?;
                (!name.contains('/')).then(|| StoredFile { name: name.into(), modified: *modified })
            })
            .collect())
    }

    fn delete(&self, rel_path: &str) -> Result<(), SinkError> {
        self.files.lock().unwrap().remove(rel_path);
        Ok(())
    }
}

fn export(sink: &Arc<MemorySink>, config: &OutputConfig, report: &ReviewReport) -> ExportOutcome {
    let input = HandoffInput { meta: meta(), report, rules: BlockingRules::default() };
    ExportReview::new(sink.clone(), config).unwrap().execute(&input)
}

const DIR: &str = "shop-api/feature-SHOP-7-pay";
const RULES_MD: &str = "shop-api/qgate-rules.md";
const RULES_STATE: &str = "shop-api/.qgate-rules.json";

/// The default config without the rules file, for tests about the per-commit files.
fn no_rules() -> OutputConfig {
    OutputConfig { rules_enabled: false, ..OutputConfig::default() }
}

#[test]
fn writes_markdown_and_latest_by_default() {
    let sink = Arc::new(MemorySink::default());
    let outcome = export(&sink, &OutputConfig::default(), &failing_report());
    assert!(outcome.is_ok(), "{:?}", outcome.errors);
    assert_eq!(
        outcome.written,
        vec![RULES_STATE.to_string(), RULES_MD.to_string(), format!("{DIR}/3f9c2ab.md"), format!("{DIR}/latest.md")]
    );
    assert_eq!(outcome.markdown_path(), Some(format!("{DIR}/3f9c2ab.md").as_str()));
    assert_eq!(sink.text(&format!("{DIR}/latest.md")), sink.text(&format!("{DIR}/3f9c2ab.md")));
    assert!(sink.text(&format!("{DIR}/latest.md")).starts_with("# qgate review: shop-api"));
}

#[test]
fn json_is_opt_in_and_stale_latest_json_is_removed() {
    let sink = Arc::new(MemorySink::default());
    let both = OutputConfig { formats: vec![OutputFormat::Markdown, OutputFormat::Json], ..no_rules() };
    let outcome = export(&sink, &both, &failing_report());
    assert_eq!(outcome.written.len(), 4);
    let json: serde_json::Value = serde_json::from_str(&sink.text(&format!("{DIR}/latest.json"))).unwrap();
    assert_eq!(json["schema"], JSON_SCHEMA);

    export(&sink, &no_rules(), &failing_report());
    assert!(!sink.paths().contains(&format!("{DIR}/latest.json")), "latest.json would be stale");
    assert!(sink.paths().contains(&format!("{DIR}/3f9c2ab.json")), "per-commit files are kept");
}

#[test]
fn passing_runs_update_latest_even_without_per_commit_files() {
    let sink = Arc::new(MemorySink::default());
    let config = OutputConfig { include_passing: false, ..no_rules() };
    let outcome = export(&sink, &config, &passing_report());
    assert_eq!(outcome.written, vec![format!("{DIR}/latest.md")]);
    assert!(sink.text(&format!("{DIR}/latest.md")).contains("PASSED"));
    let outcome = export(&sink, &config, &failing_report());
    assert_eq!(outcome.written.len(), 2, "failing runs still get their own file");
}

#[test]
fn disabled_and_no_latest() {
    let sink = Arc::new(MemorySink::default());
    let disabled = OutputConfig { enabled: false, ..no_rules() };
    assert_eq!(export(&sink, &disabled, &failing_report()).skipped, Some(SkipReason::Disabled));
    let no_latest = OutputConfig { write_latest: false, ..no_rules() };
    assert_eq!(export(&sink, &no_latest, &failing_report()).written, vec![format!("{DIR}/3f9c2ab.md")]);
    assert_eq!(sink.paths().len(), 1);
}

#[test]
fn keep_last_prunes_only_older_generated_files() {
    let sink = Arc::new(MemorySink::default());
    for (index, sha) in ["aaaaaaa", "bbbbbbb", "ccccccc"].iter().enumerate() {
        sink.put(&format!("{DIR}/{sha}.md"), 10 + index as u64);
        sink.put(&format!("{DIR}/{sha}.json"), 10 + index as u64);
    }
    let untouchable = ["notes.md", "README.md", "abc.txt", "dddddddd.md", "latest.json"];
    for name in untouchable {
        sink.put(&format!("{DIR}/{name}"), 100);
    }
    sink.put("shop-api/main/eeeeeee.md", 100);
    let config = OutputConfig { keep_last: 2, ..no_rules() };
    let outcome = export(&sink, &config, &failing_report());
    assert!(outcome.is_ok(), "{:?}", outcome.errors);
    let mut deleted = outcome.deleted.clone();
    deleted.sort();
    let expected: Vec<String> =
        ["bbbbbbb.json", "bbbbbbb.md", "ccccccc.json", "ccccccc.md"].iter().map(|n| format!("{DIR}/{n}")).collect();
    assert_eq!(deleted, expected, "the current run and the newest older run are kept");
    let paths = sink.paths();
    for name in ["3f9c2ab.md", "aaaaaaa.md", "aaaaaaa.json", "latest.md", "notes.md", "README.md", "dddddddd.md"] {
        assert!(paths.contains(&format!("{DIR}/{name}")), "{name} kept");
    }
    assert!(!paths.contains(&format!("{DIR}/latest.json")), "stale latest.json removed (JSON disabled)");
    assert!(paths.contains(&"shop-api/main/eeeeeee.md".to_string()), "other branches untouched");
}

#[test]
fn current_run_is_kept_even_with_an_older_timestamp() {
    let sink = Arc::new(MemorySink::default());
    sink.put(&format!("{DIR}/aaaaaaa.md"), 0);
    let config = OutputConfig { keep_last: 1, write_latest: false, ..no_rules() };
    // Pretend the clock went backwards: the existing file looks newer than the one we write.
    sink.files.lock().unwrap().get_mut(&format!("{DIR}/aaaaaaa.md")).unwrap().1 =
        SystemTime::UNIX_EPOCH + Duration::from_secs(9_000_000);
    let outcome = export(&sink, &config, &failing_report());
    assert_eq!(outcome.deleted, vec![format!("{DIR}/aaaaaaa.md")]);
    assert_eq!(sink.paths(), vec![format!("{DIR}/3f9c2ab.md")]);
}

#[test]
fn no_pruning_without_per_run_placeholder_or_with_keep_all() {
    let sink = Arc::new(MemorySink::default());
    sink.put(&format!("{DIR}/aaaaaaa.md"), 10);
    let keep_all = OutputConfig { keep_last: 0, ..no_rules() };
    assert!(export(&sink, &keep_all, &failing_report()).deleted.is_empty());
    let per_ticket = OutputConfig { keep_last: 1, path_template: "{repo}/{branch}/{ticket}.md".into(), ..keep_all };
    let outcome = export(&sink, &per_ticket, &failing_report());
    assert_eq!(outcome.written[0], format!("{DIR}/SHOP-7.md"));
    assert!(outcome.deleted.is_empty());
}

#[test]
fn errors_are_reported_not_raised() {
    let sink = Arc::new(MemorySink { fail_writes: true, ..MemorySink::default() });
    let outcome = export(&sink, &OutputConfig::default(), &failing_report());
    assert!(!outcome.is_ok());
    assert_eq!(outcome.errors.len(), 3, "{:?}", outcome.errors);
    assert!(outcome.errors[0].contains("disk full") && outcome.errors[0].contains(RULES_STATE));
    assert!(outcome.errors[1].contains("disk full") && outcome.errors[1].contains("3f9c2ab.md"));
    assert!(sink.paths().is_empty());
    assert!(outcome.written.is_empty());
}

#[test]
fn invalid_template_is_rejected_at_construction() {
    let config = OutputConfig { path_template: "../{sha7}.md".into(), ..OutputConfig::default() };
    assert!(ExportReview::new(Arc::new(MemorySink::default()), &config).is_err());
}

#[test]
fn placeholders_match_the_settings_contract() {
    let names: Vec<&str> = Placeholder::ALL.iter().map(|p| p.name()).collect();
    assert_eq!(names, OUTPUT_PLACEHOLDERS.to_vec());
}

#[test]
fn native_dir_is_inside_the_repository() {
    assert_eq!(
        native_default_dir(std::path::Path::new("/code/shop")),
        std::path::PathBuf::from("/code/shop/.qgate/reviews")
    );
}

#[test]
fn end_to_end_with_the_filesystem_sink() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(FsReviewFileSink::new(dir.path().join("reviews")).unwrap());
    let report = failing_report();
    let input = HandoffInput { meta: meta(), report: &report, rules: BlockingRules::default() };
    let outcome = ExportReview::new(sink.clone(), &OutputConfig::default()).unwrap().execute(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.errors);
    let latest = std::fs::read_to_string(dir.path().join("reviews").join(DIR).join("latest.md")).unwrap();
    assert!(latest.contains("## Do this first (blocking)"));
    let rules = std::fs::read_to_string(dir.path().join("reviews").join(RULES_MD)).unwrap();
    assert!(rules.contains("**[critical] Build SQL with bound parameters only.**"), "{rules}");
    assert!(dir.path().join("reviews").join(RULES_STATE).is_file());
}

#[test]
fn rules_are_merged_across_runs_and_only_new_ones_are_listed() {
    let sink = Arc::new(MemorySink::default());
    let first = export(&sink, &OutputConfig::default(), &failing_report());
    assert!(first.is_ok(), "{:?}", first.errors);
    assert_eq!(first.rules_file.as_deref(), Some(RULES_MD));
    let review = sink.text(&format!("{DIR}/3f9c2ab.md"));
    assert!(review.contains("New rules from this review, already added to `shop-api/qgate-rules.md`"), "{review}");
    assert!(review.contains("- **[critical] Build SQL with bound parameters only.**"));
    let rules = sink.text(RULES_MD);
    assert!(rules.contains("seen 1× (last `3f9c2ab`, 2026-11-20)"), "{rules}");

    // The next commit hits the same rules: counted, but nothing new to add.
    let mut later = meta();
    later.commit = "bbbbbbb1".repeat(5);
    let report = failing_report();
    let input = HandoffInput { meta: later, report: &report, rules: BlockingRules::default() };
    ExportReview::new(sink.clone(), &OutputConfig::default()).unwrap().execute(&input);
    assert!(sink.text(RULES_MD).contains("seen 2× (last `bbbbbbb`"));
    assert!(!sink.text(&format!("{DIR}/bbbbbbb.md")).contains("## Rules to add"));
    let state: serde_json::Value = serde_json::from_str(&sink.text(RULES_STATE)).unwrap();
    assert_eq!(state["schema"], "qgate.rules/v1");
    assert_eq!(state["rules"].as_array().unwrap().len(), 4);
}

#[test]
fn rules_respect_the_cap_and_the_configured_file_name() {
    let sink = Arc::new(MemorySink::default());
    let config = OutputConfig { rules_file: "AGENT_RULES.md".into(), rules_max: 1, ..OutputConfig::default() };
    export(&sink, &config, &failing_report());
    let rules = sink.text("shop-api/AGENT_RULES.md");
    assert!(rules.contains("1. **[critical]") && !rules.contains("2. "), "{rules}");
    assert!(sink.paths().contains(&"shop-api/.AGENT_RULES.json".to_string()));
}

#[test]
fn unreadable_rule_state_is_reported_and_never_overwritten() {
    let sink = Arc::new(MemorySink::default());
    sink.put(RULES_STATE, 0);
    let outcome = export(&sink, &OutputConfig::default(), &failing_report());
    assert_eq!(outcome.errors.len(), 1);
    assert!(outcome.errors[0].contains("fix or delete it"), "{:?}", outcome.errors);
    assert_eq!(sink.text(RULES_STATE), "old");
    assert!(!sink.paths().contains(&RULES_MD.to_string()));
    assert!(
        sink.text(&format!("{DIR}/3f9c2ab.md")).contains("Rules from this review; add them"),
        "all run rules listed"
    );
}

#[test]
fn rules_are_skipped_when_disabled_or_when_the_run_has_none() {
    let sink = Arc::new(MemorySink::default());
    export(&sink, &no_rules(), &failing_report());
    export(&sink, &OutputConfig::default(), &passing_report());
    assert!(!sink.paths().iter().any(|p| p.contains("qgate-rules")), "{:?}", sink.paths());
}

#[test]
fn pruning_never_deletes_the_rules_files() {
    let sink = Arc::new(MemorySink::default());
    // A rules file name that looks like a generated per-commit file in the same folder.
    let config = OutputConfig {
        path_template: "{repo}/{sha7}.md".into(),
        keep_last: 1,
        rules_file: "abcdef1.md".into(),
        ..OutputConfig::default()
    };
    sink.put("shop-api/aaaaaaa.md", 10);
    let outcome = export(&sink, &config, &failing_report());
    assert_eq!(outcome.deleted, vec!["shop-api/aaaaaaa.md".to_string()]);
    assert!(sink.paths().contains(&"shop-api/abcdef1.md".to_string()));
}

#[test]
fn html_is_opt_in_and_written_with_latest() {
    let sink = Arc::new(MemorySink::default());
    export(&sink, &no_rules(), &failing_report());
    assert!(!sink.paths().iter().any(|p| p.ends_with(".html")), "html is off by default");

    let both = OutputConfig { formats: vec![OutputFormat::Markdown, OutputFormat::Html], ..no_rules() };
    let outcome = export(&sink, &both, &failing_report());
    assert!(outcome.is_ok(), "{:?}", outcome.errors);
    assert_eq!(
        outcome.written,
        vec![
            format!("{DIR}/3f9c2ab.md"),
            format!("{DIR}/3f9c2ab.html"),
            format!("{DIR}/latest.md"),
            format!("{DIR}/latest.html")
        ]
    );
    assert!(sink.text(&format!("{DIR}/3f9c2ab.html")).starts_with("<!DOCTYPE html>"));
    assert_eq!(sink.text(&format!("{DIR}/latest.html")), sink.text(&format!("{DIR}/3f9c2ab.html")));
    assert!(sink.text(&format!("{DIR}/latest.md")).starts_with("# qgate review"), "markdown is unchanged");

    export(&sink, &no_rules(), &failing_report());
    assert!(!sink.paths().contains(&format!("{DIR}/latest.html")), "stale latest.html removed");
    assert!(sink.paths().contains(&format!("{DIR}/3f9c2ab.html")), "per-commit html is kept");
}

#[test]
fn html_only_writes_no_markdown() {
    let sink = Arc::new(MemorySink::default());
    let config = OutputConfig { formats: vec![OutputFormat::Html], ..OutputConfig::default() };
    let outcome = export(&sink, &config, &failing_report());
    assert!(outcome.is_ok(), "{:?}", outcome.errors);
    assert_eq!(outcome.markdown_path(), None);
    let paths = sink.paths();
    assert!(paths.contains(&format!("{DIR}/3f9c2ab.html")) && !paths.contains(&format!("{DIR}/3f9c2ab.md")));
    assert!(paths.contains(&RULES_MD.to_string()), "rules file is still written");
}

#[test]
fn html_is_pruned_independently_of_markdown_and_rules_files() {
    let sink = Arc::new(MemorySink::default());
    for (index, sha) in ["aaaaaaa", "bbbbbbb", "ccccccc"].iter().enumerate() {
        sink.put(&format!("{DIR}/{sha}.md"), 10 + index as u64);
    }
    // Only the two oldest runs have an html copy.
    sink.put(&format!("{DIR}/bbbbbbb.html"), 11);
    sink.put(&format!("{DIR}/ccccccc.html"), 12);
    sink.put("shop-api/qgate-rules.md", 100);
    let config = OutputConfig {
        keep_last: 2,
        formats: vec![OutputFormat::Markdown, OutputFormat::Html],
        rules_enabled: true,
        ..OutputConfig::default()
    };
    let outcome = export(&sink, &config, &passing_report());
    assert!(outcome.is_ok(), "{:?}", outcome.errors);
    let mut deleted = outcome.deleted.clone();
    deleted.sort();
    let expected: Vec<String> =
        ["bbbbbbb.md", "ccccccc.md", "ccccccc.html"].iter().map(|n| format!("{DIR}/{n}")).collect();
    let mut expected = expected;
    expected.sort();
    assert_eq!(deleted, expected, "md keeps current + newest; html keeps current + newest html");
    let paths = sink.paths();
    for kept in ["3f9c2ab.md", "3f9c2ab.html", "aaaaaaa.md", "bbbbbbb.html", "latest.html"] {
        assert!(paths.contains(&format!("{DIR}/{kept}")), "{kept} kept");
    }
    assert!(paths.contains(&"shop-api/qgate-rules.md".to_string()), "rules file untouched");
}

#[test]
fn html_files_go_through_the_filesystem_sink() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(FsReviewFileSink::new(dir.path()).unwrap());
    let config = OutputConfig { formats: vec![OutputFormat::Html], ..no_rules() };
    let input = HandoffInput { meta: meta(), report: &failing_report(), rules: BlockingRules::default() };
    let outcome = ExportReview::new(sink, &config).unwrap().execute(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.errors);
    let written = std::fs::read_to_string(dir.path().join(DIR).join("3f9c2ab.html")).unwrap();
    assert!(written.contains("verdict fail"));
}
