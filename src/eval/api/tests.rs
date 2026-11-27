use std::sync::Arc;

use futures::future::BoxFuture;
use reqwest::StatusCode;
use serde_json::Value;
use tokio::net::TcpListener;

use super::*;
use crate::eval::application::EvalError;
use crate::eval::application::test_support::{MemoryStore, provenance, sample_run};
use crate::history::application::RunProvenance;

struct FixedProvenance(RunProvenance);

impl CurrentProvenance for FixedProvenance {
    fn current<'a>(&'a self) -> BoxFuture<'a, RunProvenance> {
        Box::pin(async { self.0.clone() })
    }
}

async fn serve(store: Arc<MemoryStore>, current: RunProvenance) -> String {
    let state = EvalApiState::new(store, Arc::new(FixedProvenance(current)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });
    format!("http://{addr}")
}

async fn json(base: &str, path: &str) -> (StatusCode, Value) {
    let response = reqwest::get(format!("{base}{path}")).await.unwrap();
    (response.status(), response.json().await.unwrap())
}

fn two_runs() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.save(&sample_run(&[true, true])).unwrap();
    store.save(&sample_run(&[true, false])).unwrap();
    store
}

#[tokio::test]
async fn lists_runs_newest_first_with_a_regression_flag_and_summary() {
    let base = serve(two_runs(), provenance()).await;

    let (status, body) = json(&base, "/api/eval/runs").await;

    assert_eq!(status, StatusCode::OK);
    let runs = body["runs"].as_array().unwrap();
    assert_eq!(runs.iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), [2, 1]);
    assert_eq!(runs[0]["regression"], true);
    assert_eq!(runs[1]["regression"], false);
    assert_eq!(runs[0]["provenance"]["model"], "m");
    assert_eq!(runs[0]["summary"]["counts"]["fn"], 1);
    assert_eq!(runs[1]["summary"]["recall"], 1.0);
    assert!(runs[0].get("results").is_none(), "list items stay small");
}

#[tokio::test]
async fn limit_is_validated() {
    let base = serve(two_runs(), provenance()).await;

    assert_eq!(json(&base, "/api/eval/runs?limit=1").await.1["runs"].as_array().unwrap().len(), 1);
    let (status, body) = json(&base, "/api/eval/runs?limit=abc").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid limit");
}

#[tokio::test]
async fn a_run_has_full_metrics_results_and_a_comparison_to_the_previous_run() {
    let base = serve(two_runs(), provenance()).await;

    let (status, body) = json(&base, "/api/eval/runs/2").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], 2);
    assert_eq!(body["previous_id"], 1);
    assert_eq!(body["regression"], true);
    assert_eq!(body["comparison"]["regressed"], true);
    assert_eq!(body["comparison"]["lost_catches"][0]["case_id"], "a");
    assert_eq!(body["metrics"]["cases"][0]["case_id"], "a");
    assert_eq!(body["results"][0]["case_id"], "a");
    let (_, first) = json(&base, "/api/eval/runs/1").await;
    assert!(first["comparison"].is_null() && first["previous_id"].is_null());
}

#[tokio::test]
async fn unknown_runs_are_not_found() {
    let base = serve(two_runs(), provenance()).await;

    let (status, body) = json(&base, "/api/eval/runs/99").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not found");
    assert_eq!(reqwest::get(format!("{base}/api/eval/runs/abc")).await.unwrap().status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn status_says_whether_the_latest_eval_matches_the_current_setup() {
    let matching = serve(two_runs(), provenance()).await;
    let stale = serve(two_runs(), RunProvenance { prompt_hash: "new".into(), ..provenance() }).await;
    let empty = serve(Arc::new(MemoryStore::default()), provenance()).await;

    let (_, current) = json(&matching, "/api/eval/status").await;
    let (_, outdated) = json(&stale, "/api/eval/status").await;
    let (_, none) = json(&empty, "/api/eval/status").await;

    assert_eq!(current["matches_current"], true);
    assert_eq!(current["regression"], true);
    assert_eq!(current["latest"]["id"], 2);
    assert!(current["note"].is_null());
    assert_eq!(outdated["matches_current"], false);
    assert_eq!(outdated["note"], "no eval for the current model/prompt");
    assert_eq!(outdated["current_provenance"]["prompt_hash"], "new");
    assert!(none["latest"].is_null());
    assert_eq!(none["note"], "no eval has been run yet");
}

#[tokio::test]
async fn storage_failures_do_not_leak_details() {
    struct Broken;
    impl crate::eval::application::EvalStore for Broken {
        fn save(&self, _: &crate::eval::domain::run::EvalRun) -> Result<i64, EvalError> {
            Err(EvalError::Storage("secret path".into()))
        }
        fn get(&self, _: i64) -> Result<Option<crate::eval::domain::run::EvalRun>, EvalError> {
            Err(EvalError::Storage("secret path".into()))
        }
        fn latest(&self) -> Result<Option<crate::eval::domain::run::EvalRun>, EvalError> {
            Err(EvalError::Storage("secret path".into()))
        }
        fn previous_to(&self, _: i64) -> Result<Option<crate::eval::domain::run::EvalRun>, EvalError> {
            Err(EvalError::Storage("secret path".into()))
        }
        fn list(&self, _: u32) -> Result<Vec<crate::eval::domain::run::EvalRun>, EvalError> {
            Err(EvalError::Storage("secret path".into()))
        }
    }
    let state = EvalApiState::new(Arc::new(Broken), Arc::new(FixedProvenance(provenance())));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router(state)).await });

    for path in ["/api/eval/runs", "/api/eval/runs/1", "/api/eval/status"] {
        let (status, body) = json(&base, path).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        assert_eq!(body["error"], "internal error");
    }
}

#[tokio::test]
async fn the_page_is_served_and_has_no_external_resources_or_html_sinks() {
    let base = serve(two_runs(), provenance()).await;

    let response = reqwest::get(format!("{base}/eval")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let html = response.text().await.unwrap();
    for forbidden in [
        "://",
        "@import",
        "<link",
        "src=",
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "javascript:",
        "__QGATE_TOKEN__",
    ] {
        assert!(!html.contains(forbidden), "page must not contain {forbidden}");
    }
    assert!(html.contains("/api/eval/status") && html.contains("qgate eval"));
}
