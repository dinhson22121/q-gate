use std::collections::HashMap;
use std::sync::Mutex;

use futures::future::BoxFuture;

use super::*;
use crate::tickets::domain::TicketLink;

fn key(raw: &str) -> TicketKey {
    TicketKey::parse(raw).unwrap()
}

fn ticket(raw: &str, issue_type: &str, parent: Option<(&str, &str)>, criteria: &[&str]) -> Ticket {
    let mut ticket = Ticket::new(key(raw), format!("Title {raw}"));
    ticket.issue_type = Some(issue_type.into());
    ticket.parent =
        parent.map(|(k, t)| ParentRef { key: key(k), issue_type: Some(t.to_string()).filter(|t| !t.is_empty()) });
    ticket.criteria = criteria.iter().map(|c| c.to_string()).collect();
    ticket.description = format!("Description of {raw}");
    ticket.updated = Some("v1".into());
    ticket
}

#[derive(Default)]
struct FakeSource {
    tickets: Mutex<HashMap<String, Result<Ticket, TicketError>>>,
    updated: Mutex<Option<Result<Option<String>, TicketError>>>,
    fetched: Mutex<Vec<String>>,
    probed: Mutex<Vec<String>>,
}

impl FakeSource {
    fn with(tickets: Vec<Ticket>) -> Self {
        let source = Self::default();
        for t in tickets {
            source.tickets.lock().unwrap().insert(t.key.to_string(), Ok(t));
        }
        source
    }

    fn fail(&self, raw: &str, error: TicketError) {
        self.tickets.lock().unwrap().insert(raw.into(), Err(error));
    }

    fn fetched(&self) -> Vec<String> {
        self.fetched.lock().unwrap().clone()
    }
}

impl TicketSource for &FakeSource {
    async fn fetch(&self, key: &TicketKey) -> Result<Ticket, TicketError> {
        self.fetched.lock().unwrap().push(key.to_string());
        self.tickets.lock().unwrap().get(key.as_str()).cloned().unwrap_or(Err(TicketError::NotFound(key.to_string())))
    }

    async fn fetch_updated(&self, key: &TicketKey) -> Result<Option<String>, TicketError> {
        self.probed.lock().unwrap().push(key.to_string());
        self.updated.lock().unwrap().clone().unwrap_or(Ok(None))
    }
}

#[derive(Default)]
struct MemoryCache {
    entries: Mutex<HashMap<String, CachedTicket>>,
    broken: bool,
}

impl TicketCache for MemoryCache {
    fn get_ticket<'a>(&'a self, key: &'a TicketKey) -> BoxFuture<'a, Result<Option<CachedTicket>, CacheError>> {
        let result = if self.broken {
            Err(CacheError("disk full".into()))
        } else {
            Ok(self.entries.lock().unwrap().get(key.as_str()).cloned())
        };
        Box::pin(async move { result })
    }

    fn put_ticket<'a>(&'a self, entry: &'a CachedTicket) -> BoxFuture<'a, Result<(), CacheError>> {
        if self.broken {
            return Box::pin(async { Err(CacheError("disk full".into())) });
        }
        self.entries.lock().unwrap().insert(entry.ticket.key.to_string(), entry.clone());
        Box::pin(async { Ok(()) })
    }
}

fn options() -> LoadOptions {
    LoadOptions::default()
}

fn plain_notes(loaded: &LoadedTickets) -> Vec<&String> {
    loaded.notes.iter().filter(|n| !n.starts_with("completeness: ")).collect()
}

fn ids(loaded: &LoadedTickets) -> Vec<String> {
    loaded.requirements.criteria.iter().map(|c| c.id.clone()).collect()
}

#[tokio::test]
async fn subtask_merges_parent_story_and_stops_before_epic() {
    let mut sub = ticket("SHOP-8", "Sub-task", Some(("SHOP-7", "Story")), &["sub ac"]);
    sub.design_links = vec!["https://www.figma.com/design/Sub/x".into(), "https://www.figma.com/design/St/y".into()];
    let mut story = ticket("SHOP-7", "Story", Some(("SHOP-1", "Epic")), &["story ac 1", "story ac 2"]);
    story.design_links = vec!["https://www.figma.com/design/St/y".into()];
    let source = FakeSource::with(vec![sub, story, ticket("SHOP-1", "Epic", None, &["epic ac"])]);

    let loaded = LoadTicketContext::new(&source, options()).execute(&[key("SHOP-8")]).await.unwrap();

    assert_eq!(ids(&loaded), vec!["SHOP-8/AC-1", "SHOP-7/AC-1", "SHOP-7/AC-2"]);
    assert_eq!(loaded.requirements.key, "SHOP-8");
    assert_eq!(loaded.requirements.title, "Title SHOP-8");
    assert_eq!(loaded.context.keys, vec!["SHOP-8", "SHOP-7"]);
    assert_eq!(
        loaded.context.design_links,
        vec!["https://www.figma.com/design/St/y", "https://www.figma.com/design/Sub/x"]
    );
    assert_eq!(
        loaded.context.description,
        "[SHOP-8] Title SHOP-8\nDescription of SHOP-8\n\n[SHOP-7] Title SHOP-7\nDescription of SHOP-7"
    );
    assert_eq!(source.fetched(), vec!["SHOP-8", "SHOP-7"]);
    assert!(plain_notes(&loaded).is_empty());
}

#[tokio::test]
async fn walks_two_hops_through_a_task_and_skips_untyped_epic() {
    let sub = ticket("AB-3", "Sub-task", Some(("AB-2", "Task")), &[]);
    let task = ticket("AB-2", "Task", Some(("AB-1", "")), &["task ac"]);
    let source = FakeSource::with(vec![sub, task, ticket("AB-1", "Epic", None, &["epic ac"])]);

    let loaded = LoadTicketContext::new(&source, options()).execute(&[key("AB-3")]).await.unwrap();

    assert_eq!(ids(&loaded), vec!["AB-2/AC-1"]);
    assert_eq!(loaded.context.keys, vec!["AB-3", "AB-2"]);
    assert_eq!(source.fetched(), vec!["AB-3", "AB-2", "AB-1"]);
}

#[tokio::test]
async fn several_keys_share_a_story_once_and_join_titles() {
    let source = FakeSource::with(vec![
        ticket("SHOP-8", "Sub-task", Some(("SHOP-7", "Story")), &["a"]),
        ticket("SHOP-9", "Sub-task", Some(("SHOP-7", "Story")), &["b"]),
        ticket("SHOP-7", "Story", None, &["s"]),
    ]);

    let loaded = LoadTicketContext::new(&source, options()).execute(&[key("SHOP-8"), key("SHOP-9")]).await.unwrap();

    assert_eq!(ids(&loaded), vec!["SHOP-8/AC-1", "SHOP-9/AC-1", "SHOP-7/AC-1"]);
    assert_eq!(loaded.requirements.title, "SHOP-8: Title SHOP-8 | SHOP-9: Title SHOP-9");
    assert_eq!(source.fetched(), vec!["SHOP-8", "SHOP-9", "SHOP-7"]);
}

#[tokio::test]
async fn story_and_implemented_parent_are_not_walked_again() {
    let source = FakeSource::with(vec![
        ticket("SHOP-8", "Sub-task", Some(("SHOP-7", "Story")), &["a"]),
        ticket("SHOP-7", "Story", Some(("SHOP-1", "Epic")), &["s"]),
    ]);

    let loaded = LoadTicketContext::new(&source, options()).execute(&[key("SHOP-8"), key("SHOP-7")]).await.unwrap();

    assert_eq!(loaded.context.keys, vec!["SHOP-8", "SHOP-7"]);
    assert_eq!(source.fetched(), vec!["SHOP-8", "SHOP-7"]);
}

#[tokio::test]
async fn options_disable_parent_walk_and_description() {
    let source = FakeSource::with(vec![ticket("SHOP-8", "Sub-task", Some(("SHOP-7", "Story")), &["a"])]);
    let options = LoadOptions { include_parent_story: false, include_description: false, ..options() };

    let loaded = LoadTicketContext::new(&source, options).execute(&[key("SHOP-8")]).await.unwrap();

    assert_eq!(loaded.context.keys, vec!["SHOP-8"]);
    assert!(loaded.context.description.is_empty());
}

#[tokio::test]
async fn unknown_hierarchy_and_failed_parent_become_notes() {
    let mut untyped = Ticket::new(key("MCP-1"), "From MCP");
    untyped.criteria = vec!["x".into()];
    let source = FakeSource::with(vec![untyped, ticket("AB-2", "Sub-task", Some(("AB-1", "Story")), &[])]);
    source.fail("AB-1", TicketError::Unauthorized("no".into()));

    let loaded = LoadTicketContext::new(&source, options()).execute(&[key("MCP-1"), key("AB-2")]).await.unwrap();

    let notes = plain_notes(&loaded);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[0].contains("MCP-1") && notes[0].contains("no issue type"));
    assert!(notes[1].contains("parent AB-1 of AB-2"));
}

#[tokio::test]
async fn fails_only_when_no_implemented_ticket_loads() {
    let source = FakeSource::with(vec![ticket("AB-2", "Story", None, &[])]);
    let use_case = LoadTicketContext::new(&source, options());

    let partial = use_case.execute(&[key("AB-9"), key("AB-2")]).await.unwrap();
    assert_eq!(partial.requirements.key, "AB-2");
    assert!(partial.notes[0].contains("AB-9"));

    assert_eq!(use_case.execute(&[key("AB-9")]).await.unwrap_err(), TicketError::NotFound("AB-9".into()));
    assert!(matches!(use_case.execute(&[]).await, Err(TicketError::Invalid(_))));
}

fn linked(mut ticket: Ticket, qa: &str, link_type: &str) -> Ticket {
    ticket.links.push(TicketLink { key: key(qa), link_type: link_type.into() });
    ticket
}

#[tokio::test]
async fn follows_the_qa_link_type_case_insensitively_and_merges_criteria() {
    let mut dev = linked(ticket("SHOP-8", "Story", None, &["dev ac"]), "QA-1", "Is Tested By");
    dev.links.push(TicketLink { key: key("AB-5"), link_type: "relates to".into() });
    dev.doc_links = vec!["https://wiki.example/doc".into(), "https://wiki.example/doc".into()];
    let mut qa = ticket("QA-1", "Test", None, &["qa ac"]);
    qa.design_links = vec!["https://www.figma.com/design/Qa/x".into()];
    let source = FakeSource::with(vec![dev, qa, ticket("AB-5", "Task", None, &["other"])]);

    let loaded = LoadTicketContext::new(&source, options()).execute(&[key("SHOP-8")]).await.unwrap();

    assert_eq!(ids(&loaded), vec!["SHOP-8/AC-1", "QA-1/AC-1"]);
    assert_eq!(loaded.context.keys, vec!["SHOP-8", "QA-1"]);
    assert!(loaded.context.description.contains("[QA-1] Title QA-1"));
    assert_eq!(loaded.context.design_links, vec!["https://www.figma.com/design/Qa/x"]);
    assert_eq!(source.fetched(), vec!["SHOP-8", "QA-1"]);
    assert!(loaded.notes.contains(&"QA ticket QA-1 linked via 'Is Tested By' merged".to_string()));
    assert_eq!(loaded.notes.iter().filter(|n| *n == "dev doc: https://wiki.example/doc").count(), 1);
    assert!(!loaded.notes.contains(&"completeness: no linked QA ticket".to_string()));
}

#[tokio::test]
async fn failed_qa_load_is_a_note_and_duplicates_are_skipped() {
    let mut dev = linked(ticket("SHOP-8", "Story", None, &["a"]), "QA-9", "is tested by");
    dev.links.push(TicketLink { key: key("QA-9"), link_type: "is tested by".into() });
    dev.links.push(TicketLink { key: key("SHOP-8"), link_type: "is tested by".into() });
    let source = FakeSource::with(vec![dev]);
    source.fail("QA-9", TicketError::NotFound("QA-9".into()));

    let loaded = LoadTicketContext::new(&source, options()).execute(&[key("SHOP-8")]).await.unwrap();

    assert_eq!(loaded.context.keys, vec!["SHOP-8"]);
    assert_eq!(source.fetched(), vec!["SHOP-8", "QA-9"]);
    assert!(loaded.notes.iter().any(|n| n.contains("QA ticket QA-9 of SHOP-8 could not be loaded")));
    assert!(loaded.notes.contains(&"completeness: no linked QA ticket".to_string()));
}

#[tokio::test]
async fn qa_links_are_ignored_when_no_link_type_is_configured() {
    let source = FakeSource::with(vec![
        linked(ticket("SHOP-8", "Story", None, &["a"]), "QA-1", "is tested by"),
        ticket("QA-1", "Test", None, &["qa"]),
    ]);
    let options = LoadOptions { qa_link_type: None, ..options() };

    let loaded = LoadTicketContext::new(&source, options).execute(&[key("SHOP-8")]).await.unwrap();

    assert_eq!(source.fetched(), vec!["SHOP-8"]);
    assert_eq!(loaded.context.keys, vec!["SHOP-8"]);
}

#[tokio::test]
async fn thin_ticket_reports_completeness_gaps_as_notes() {
    let source = FakeSource::with(vec![ticket("AB-1", "Story", None, &[])]);

    let loaded = LoadTicketContext::new(&source, options()).execute(&[key("AB-1")]).await.unwrap();

    assert_eq!(
        loaded.notes,
        vec![
            "completeness: no acceptance criteria",
            "completeness: thin or missing description",
            "completeness: no Figma design link",
            "completeness: no linked QA ticket",
        ]
    );
}

#[test]
fn long_descriptions_are_truncated_by_chars() {
    let text = "é".repeat(MAX_DESCRIPTION_CHARS + 5);
    let truncated = truncate_chars(&text, MAX_DESCRIPTION_CHARS);
    assert_eq!(truncated.chars().count(), MAX_DESCRIPTION_CHARS + TRUNCATION_MARKER.chars().count());
    assert_eq!(truncate_chars("short", 10), "short");
}

struct Harness {
    source: FakeSource,
    cache: Arc<MemoryCache>,
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl Harness {
    fn new() -> Self {
        let source = FakeSource::with(vec![ticket("AB-1", "Story", None, &["a"])]);
        let now = Arc::new(Mutex::new(DateTime::parse_from_rfc3339("2026-11-20T10:00:00Z").unwrap().to_utc()));
        Self { source, cache: Arc::new(MemoryCache::default()), now }
    }

    fn use_case(&self) -> LoadTicketContext<&FakeSource> {
        let now = self.now.clone();
        LoadTicketContext::new(&self.source, options())
            .with_cache(self.cache.clone())
            .with_clock(Arc::new(move || *now.lock().unwrap()))
    }

    fn advance(&self, minutes: i64) {
        *self.now.lock().unwrap() += Duration::minutes(minutes);
    }

    fn set_criteria(&self, criteria: &[&str], updated: &str) {
        let mut fresh = ticket("AB-1", "Story", None, criteria);
        fresh.updated = Some(updated.into());
        self.source.tickets.lock().unwrap().insert("AB-1".into(), Ok(fresh));
    }

    fn cached_at(&self) -> DateTime<Utc> {
        self.cache.entries.lock().unwrap()["AB-1"].fetched_at
    }

    async fn criteria(&self) -> Vec<String> {
        let loaded = self.use_case().execute(&[key("AB-1")]).await.unwrap();
        loaded.requirements.criteria.into_iter().map(|c| c.text).collect()
    }
}

#[tokio::test]
async fn cached_ticket_is_reused_within_ttl() {
    let h = Harness::new();
    assert_eq!(h.criteria().await, vec!["a"]);
    h.set_criteria(&["changed"], "v2");
    h.advance(9);

    assert_eq!(h.criteria().await, vec!["a"]);
    assert_eq!(h.source.fetched(), vec!["AB-1"]);
    assert!(h.source.probed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn after_ttl_unchanged_updated_keeps_cached_parse_and_refreshes_timestamp() {
    let h = Harness::new();
    h.criteria().await;
    h.set_criteria(&["changed"], "v1");
    *h.source.updated.lock().unwrap() = Some(Ok(Some("v1".into())));
    h.advance(11);

    assert_eq!(h.criteria().await, vec!["a"]);
    assert_eq!(h.source.fetched(), vec!["AB-1"]);
    assert_eq!(h.cached_at(), *h.now.lock().unwrap());
}

#[tokio::test]
async fn after_ttl_changed_or_unknown_updated_refetches() {
    let h = Harness::new();
    h.criteria().await;
    h.set_criteria(&["changed"], "v2");
    *h.source.updated.lock().unwrap() = Some(Ok(Some("v2".into())));
    h.advance(11);
    assert_eq!(h.criteria().await, vec!["changed"]);

    h.set_criteria(&["again"], "v3");
    *h.source.updated.lock().unwrap() = Some(Ok(None));
    h.advance(11);
    assert_eq!(h.criteria().await, vec!["again"]);
    assert_eq!(h.source.fetched(), vec!["AB-1", "AB-1", "AB-1"]);
    assert_eq!(h.cache.entries.lock().unwrap()["AB-1"].jira_updated.as_deref(), Some("v3"));
}

#[tokio::test]
async fn tracker_outage_after_ttl_uses_stale_copy_with_note() {
    let h = Harness::new();
    h.criteria().await;
    h.advance(11);
    *h.source.updated.lock().unwrap() = Some(Err(TicketError::Unavailable("down".into())));
    let loaded = h.use_case().execute(&[key("AB-1")]).await.unwrap();
    assert!(loaded.notes[0].contains("using the copy cached at 2026-11-20 10:00 UTC"), "{:?}", loaded.notes);

    *h.source.updated.lock().unwrap() = Some(Ok(None));
    h.source.fail("AB-1", TicketError::Unavailable("down".into()));
    let loaded = h.use_case().execute(&[key("AB-1")]).await.unwrap();
    assert_eq!(loaded.requirements.criteria.len(), 1);
    assert!(loaded.notes[0].contains("cached"));

    h.source.fail("AB-1", TicketError::NotFound("AB-1".into()));
    assert!(h.use_case().execute(&[key("AB-1")]).await.is_err());
}

#[tokio::test]
async fn broken_cache_falls_back_to_tracker_with_note() {
    let source = FakeSource::with(vec![ticket("AB-1", "Story", None, &["a"])]);
    let cache = Arc::new(MemoryCache { broken: true, ..MemoryCache::default() });

    let loaded = LoadTicketContext::new(&source, options()).with_cache(cache).execute(&[key("AB-1")]).await.unwrap();

    assert_eq!(loaded.requirements.criteria.len(), 1);
    let notes = plain_notes(&loaded);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("disk full"));
}
