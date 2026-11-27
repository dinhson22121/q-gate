use std::sync::Mutex;

use super::*;
use crate::cache::application::store::fakes::MemoryCache;
use crate::review::domain::change_set::fixtures::{added, file};
use crate::review::domain::finding::Severity;
use crate::review::domain::finding::fixtures::finding;
use crate::review::domain::requirements::Requirements;

#[derive(Default)]
struct CountingReviewer {
    calls: Mutex<u32>,
    assessments: Mutex<u32>,
    fail: bool,
}

impl CodeReviewer for CountingReviewer {
    async fn review(&self, _input: ReviewInput<'_>) -> Result<ReviewerOutput, ReviewerError> {
        *self.calls.lock().unwrap() += 1;
        if self.fail {
            return Err(ReviewerError::Unavailable("overloaded".into()));
        }
        Ok(ReviewerOutput {
            findings: vec![finding("a.go", 1, Severity::Major, "x := 1")],
            cost_usd: Some(0.5),
            ..ReviewerOutput::default()
        })
    }

    async fn assess_requirements(&self, _input: RequirementsInput<'_>) -> Result<AssessmentOutput, ReviewerError> {
        *self.assessments.lock().unwrap() += 1;
        Ok(AssessmentOutput::default())
    }
}

fn tags() -> CacheTags {
    CacheTags { model: "m".into(), effort: "high".into(), prompt: "p1".into(), toolchain: "t1".into() }
}

fn reviewer(
    cache: &Arc<MemoryCache>,
    tags: CacheTags,
    no_cache: bool,
    fail: bool,
) -> CachingCodeReviewer<CountingReviewer> {
    CachingCodeReviewer::new(CountingReviewer { fail, ..CountingReviewer::default() }, cache.clone(), tags, no_cache)
}

fn files(content: &str) -> Vec<FileChange> {
    vec![file("a.go", 1, vec![added(1, "x := 1")], Some(content))]
}

async fn review(r: &CachingCodeReviewer<CountingReviewer>, files: &[FileChange], guidelines: &str) -> ReviewerOutput {
    r.review(ReviewInput { files, guidelines, callers: &[] }).await.unwrap()
}

fn calls(r: &CachingCodeReviewer<CountingReviewer>) -> u32 {
    *r.inner.calls.lock().unwrap()
}

#[tokio::test]
async fn identical_batch_is_served_from_cache_without_cost() {
    let cache: Arc<MemoryCache> = Arc::default();
    let first = reviewer(&cache, tags(), false, false);
    let fresh = review(&first, &files("x := 1\n"), "g").await;
    assert_eq!(fresh.cost_usd, Some(0.5));

    let second = reviewer(&cache, tags(), false, false);
    let hit = review(&second, &files("x := 1\n"), "g").await;
    assert_eq!(calls(&second), 0);
    assert_eq!(hit.findings, fresh.findings);
    assert_eq!(hit.cost_usd, None);
}

#[tokio::test]
async fn changed_content_guidelines_model_or_prompt_is_a_miss() {
    let cache: Arc<MemoryCache> = Arc::default();
    review(&reviewer(&cache, tags(), false, false), &files("x := 1\n"), "g").await;
    let cases = [
        (tags(), files("x := 2\n"), "g"),
        (tags(), files("x := 1\n"), "other guidelines"),
        (CacheTags { model: "m2".into(), ..tags() }, files("x := 1\n"), "g"),
        (CacheTags { effort: "low".into(), ..tags() }, files("x := 1\n"), "g"),
        (CacheTags { prompt: "p2".into(), ..tags() }, files("x := 1\n"), "g"),
        (CacheTags { toolchain: "t2".into(), ..tags() }, files("x := 1\n"), "g"),
    ];
    for (tags, files, guidelines) in cases {
        let r = reviewer(&cache, tags, false, false);
        review(&r, &files, guidelines).await;
        assert_eq!(calls(&r), 1);
    }
}

#[tokio::test]
async fn a_different_file_set_in_the_batch_is_a_miss() {
    let cache: Arc<MemoryCache> = Arc::default();
    review(&reviewer(&cache, tags(), false, false), &files("x := 1\n"), "g").await;
    let mut two = files("x := 1\n");
    two.push(file("b.go", 1, vec![added(1, "y := 2")], Some("y := 2\n")));
    let r = reviewer(&cache, tags(), false, false);
    review(&r, &two, "g").await;
    assert_eq!(calls(&r), 1);
}

#[tokio::test]
async fn errors_are_not_cached_and_no_cache_bypasses() {
    let cache: Arc<MemoryCache> = Arc::default();
    let failing = reviewer(&cache, tags(), false, true);
    assert!(failing.review(ReviewInput { files: &files("x"), guidelines: "", callers: &[] }).await.is_err());
    assert!(cache.map.lock().unwrap().is_empty());

    let bypass = reviewer(&cache, tags(), true, false);
    review(&bypass, &files("x"), "").await;
    review(&bypass, &files("x"), "").await;
    assert_eq!(calls(&bypass), 2);
    assert!(cache.map.lock().unwrap().is_empty());
}

#[tokio::test]
async fn requirements_assessment_always_reaches_the_inner_reviewer() {
    let cache: Arc<MemoryCache> = Arc::default();
    let r = reviewer(&cache, tags(), false, false);
    let requirements = Requirements { key: "K".into(), title: "t".into(), url: None, criteria: Vec::new() };
    for _ in 0..2 {
        r.assess_requirements(RequirementsInput {
            files: &[],
            requirements: &requirements,
            clarifications: &[],
            expanded: false,
        })
        .await
        .unwrap();
    }
    assert_eq!(*r.inner.assessments.lock().unwrap(), 2);
}

#[tokio::test]
async fn different_callers_miss_the_cache_and_same_callers_hit_it() {
    use crate::review::domain::callers::CallerSite;
    let cache = Arc::new(MemoryCache::default());
    let r = reviewer(&cache, tags(), false, false);
    let site = |line: u32| CallerSite {
        symbol: "Total".into(),
        defined_in: "a.go".into(),
        file: "api.go".into(),
        line,
        snippet: "Total()".into(),
    };
    let files = files("x := 1");
    r.review(ReviewInput { files: &files, guidelines: "", callers: &[site(1)] }).await.unwrap();
    r.review(ReviewInput { files: &files, guidelines: "", callers: &[site(1)] }).await.unwrap();
    assert_eq!(calls(&r), 1);
    r.review(ReviewInput { files: &files, guidelines: "", callers: &[site(2)] }).await.unwrap();
    assert_eq!(calls(&r), 2);
}
