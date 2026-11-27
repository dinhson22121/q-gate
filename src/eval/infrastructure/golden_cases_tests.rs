use std::path::PathBuf;

use super::{FsCaseRepository, GitWorkspace};
use crate::eval::application::{CaseRepository, CaseWorkspace};

const SHIPPED_CASES: [&str; 10] = [
    "go-kan4-login",
    "go-kan5-sqli-fixed",
    "go-kan6-traversal-fixed",
    "go-kan7-items",
    "go-kan7-items-missing-validation",
    "go-shop4-injection-traversal",
    "py-sqli-fstring",
    "py-sqli-parameterized",
    "ts-command-injection",
    "ts-execfile-safe",
];

fn cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("eval/cases")
}

#[test]
fn the_shipped_golden_set_is_complete_and_valid() {
    let repo = FsCaseRepository::new(cases_dir());

    assert_eq!(repo.ids().unwrap(), SHIPPED_CASES);
    for id in SHIPPED_CASES {
        let loaded = repo.load(id).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(!loaded.case.description.is_empty(), "{id} needs a description");
    }
}

#[tokio::test]
async fn every_shipped_patch_applies_and_every_label_points_into_the_changed_file() {
    let repo = FsCaseRepository::new(cases_dir());
    for id in repo.ids().unwrap() {
        let loaded = repo.load(&id).unwrap();

        let workspace = GitWorkspace.materialize(&loaded).await.unwrap_or_else(|e| panic!("{id}: {e}"));

        let lines_of = |file: &str| {
            std::fs::read_to_string(workspace.root.join(file))
                .unwrap_or_else(|_| panic!("{id}: {file} is not in the changed tree"))
                .lines()
                .count() as u32
        };
        for expected in &loaded.case.expected {
            assert!(
                expected.line <= lines_of(&expected.file),
                "{id}: {} is past the end of the file",
                expected.label()
            );
        }
        for forbidden in &loaded.case.forbidden {
            assert!(
                forbidden.line <= lines_of(&forbidden.file),
                "{id}: forbidden line {} is past the end",
                forbidden.line
            );
        }
    }
}

#[test]
fn the_golden_set_mixes_vulnerable_safe_and_criteria_cases() {
    let repo = FsCaseRepository::new(cases_dir());
    let cases: Vec<_> = repo.ids().unwrap().iter().map(|id| repo.load(id).unwrap().case).collect();

    assert!(cases.iter().any(|c| !c.expected.is_empty()));
    assert!(cases.iter().any(|c| !c.forbidden.is_empty()));
    assert!(cases.iter().any(|c| c.labelled_criteria() > 0));
    let languages: std::collections::BTreeSet<_> = cases.iter().map(|c| c.language.as_str()).collect();
    assert_eq!(languages.into_iter().collect::<Vec<_>>(), ["go", "python", "typescript"]);
}
