use std::collections::HashMap;
use std::sync::Mutex;

use futures::future::BoxFuture;

use super::*;
use crate::review::application::agent_ports::AgentError;
use crate::tickets::application::CacheError;

struct FakeInferrer {
    answer: Result<Vec<String>, AgentError>,
    calls: Mutex<usize>,
}

impl FakeInferrer {
    fn answering(texts: &[&str]) -> Arc<Self> {
        Arc::new(Self { answer: Ok(texts.iter().map(|t| t.to_string()).collect()), calls: Mutex::new(0) })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self { answer: Err(AgentError("model down".into())), calls: Mutex::new(0) })
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl CriteriaInferrer for FakeInferrer {
    fn infer<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<Vec<String>, AgentError>> {
        *self.calls.lock().unwrap() += 1;
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

#[derive(Default)]
struct MemoryCache {
    entries: Mutex<HashMap<String, Vec<String>>>,
    broken: bool,
}

impl CriteriaCache for MemoryCache {
    fn get_criteria<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, Result<Option<Vec<String>>, CacheError>> {
        let result = if self.broken {
            Err(CacheError("disk".into()))
        } else {
            Ok(self.entries.lock().unwrap().get(hash).cloned())
        };
        Box::pin(async move { result })
    }

    fn put_criteria<'a>(&'a self, hash: &'a str, criteria: &'a [String]) -> BoxFuture<'a, Result<(), CacheError>> {
        let result = if self.broken {
            Err(CacheError("disk".into()))
        } else {
            self.entries.lock().unwrap().insert(hash.to_string(), criteria.to_vec());
            Ok(())
        };
        Box::pin(async move { result })
    }
}

const LOGIN: &str = "login:\n- correct username and password logs the user in\n- wrong ones show Invalid credential";

#[tokio::test]
async fn inferred_criteria_get_advisory_ids_and_a_note() {
    let inferrer =
        FakeInferrer::answering(&["Correct credentials -> login succeeds", "Wrong credentials -> Invalid credential"]);
    let use_case = InferCriteria::new(inferrer, Arc::new(MemoryCache::default()));

    let result = use_case.execute("KAN-9", "Login", LOGIN).await;

    let ids: Vec<_> = result.criteria.iter().map(|c| (c.id.as_str(), c.inferred)).collect();
    assert_eq!(ids, vec![("KAN-9/AC-I1", true), ("KAN-9/AC-I2", true)]);
    assert!(result.note.unwrap().contains("2 were inferred"));
}

#[tokio::test]
async fn an_empty_description_is_not_sent_to_the_model() {
    let inferrer = FakeInferrer::answering(&["x"]);
    let use_case = InferCriteria::new(inferrer.clone(), Arc::new(MemoryCache::default()));

    let result = use_case.execute("KAN-9", "Login", "  \n").await;

    assert_eq!(result, InferredCriteria::default());
    assert_eq!(inferrer.calls(), 0);
}

#[tokio::test]
async fn the_same_description_is_inferred_once_and_then_read_from_cache() {
    let inferrer = FakeInferrer::answering(&["a"]);
    let use_case = InferCriteria::new(inferrer.clone(), Arc::new(MemoryCache::default()));

    use_case.execute("KAN-9", "Login", LOGIN).await;
    let second = use_case.execute("KAN-9", "Login", LOGIN).await;

    assert_eq!(inferrer.calls(), 1);
    assert_eq!(second.criteria[0].text, "a");
}

#[tokio::test]
async fn output_is_trimmed_deduplicated_and_capped() {
    let mut raw: Vec<String> = vec!["  same   thing ".into(), "same thing".into(), "".into()];
    raw.extend((0..20).map(|i| format!("c{i}")));
    let inferrer = Arc::new(FakeInferrer { answer: Ok(raw), calls: Mutex::new(0) });
    let use_case = InferCriteria::new(inferrer, Arc::new(MemoryCache::default()));

    let result = use_case.execute("K-1", "t", LOGIN).await;

    assert_eq!(result.criteria.len(), MAX_INFERRED_CRITERIA);
    assert_eq!(result.criteria[0].text, "same thing");
    assert_eq!(result.criteria[1].text, "c0");
}

#[tokio::test]
async fn nothing_inferred_means_no_criteria_and_no_note() {
    let use_case = InferCriteria::new(FakeInferrer::answering(&[]), Arc::new(MemoryCache::default()));

    let result = use_case.execute("K-1", "t", LOGIN).await;

    assert_eq!(result, InferredCriteria::default());
}

#[tokio::test]
async fn a_model_failure_becomes_a_note_without_criteria() {
    let use_case = InferCriteria::new(FakeInferrer::failing(), Arc::new(MemoryCache::default()));

    let result = use_case.execute("K-1", "t", LOGIN).await;

    assert!(result.criteria.is_empty());
    assert!(result.note.unwrap().contains("model down"));
}

#[tokio::test]
async fn a_broken_cache_still_infers() {
    let inferrer = FakeInferrer::answering(&["a"]);
    let use_case =
        InferCriteria::new(inferrer.clone(), Arc::new(MemoryCache { broken: true, ..MemoryCache::default() }));

    let result = use_case.execute("K-1", "t", LOGIN).await;

    assert_eq!(result.criteria.len(), 1);
    assert_eq!(inferrer.calls(), 1);
}
