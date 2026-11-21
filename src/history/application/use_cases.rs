use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use super::ports::{HistoryError, RunFilter, RunRepository};
use crate::history::domain::{
    PeriodStats, RECURRING_THRESHOLD, RecurringIssue, ReviewRun, RunListItem, RunStatus, StoredRun, TicketKey,
    TicketProgress, find_recurring, mentions_ticket, period_stats, ticket_progress,
};

pub const DEFAULT_PAGE_SIZE: u32 = 50;
pub const MAX_PAGE_SIZE: u32 = 200;
pub const RECURRING_WINDOW_DAYS: i64 = 90;
/// Most recent runs considered for one ticket.
pub const MAX_TICKET_RUNS: u32 = 200;
const SHORT_PERIOD_DAYS: i64 = 7;
const LONG_PERIOD_DAYS: i64 = 30;

/// Persist a finished (or failed) review run.
pub struct RecordRun {
    repository: Arc<dyn RunRepository>,
}

impl RecordRun {
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self { repository }
    }

    pub fn execute(&self, run: &ReviewRun) -> Result<i64, HistoryError> {
        if run.repo_path.trim().is_empty() || run.repo_name.trim().is_empty() {
            return Err(HistoryError::InvalidInput("repo_path and repo_name are required".into()));
        }
        if run.status == RunStatus::Error && run.error.is_none() {
            return Err(HistoryError::InvalidInput("an errored run needs an error message".into()));
        }
        self.repository.insert(run)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunPage {
    pub items: Vec<RunListItem>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

pub struct ListRuns {
    repository: Arc<dyn RunRepository>,
}

impl ListRuns {
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self { repository }
    }

    /// `limit` defaults to [`DEFAULT_PAGE_SIZE`] and is clamped to 1..=[`MAX_PAGE_SIZE`].
    pub fn execute(
        &self,
        repo: Option<String>,
        status: Option<RunStatus>,
        limit: Option<u32>,
        offset: u32,
    ) -> Result<RunPage, HistoryError> {
        let filter = RunFilter {
            repo: repo.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()),
            status,
            limit: limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE),
            offset,
        };
        let runs = self.repository.list(&filter)?;
        Ok(RunPage {
            items: runs.iter().map(RunListItem::from).collect(),
            total: self.repository.count(&filter)?,
            limit: filter.limit,
            offset: filter.offset,
        })
    }
}

/// The newest runs with their full reports, newest first (e.g. to replay a gate policy over them).
pub struct RecentRuns {
    repository: Arc<dyn RunRepository>,
}

impl RecentRuns {
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self { repository }
    }

    /// `limit` is clamped to 1..=[`MAX_PAGE_SIZE`].
    pub fn execute(&self, limit: u32) -> Result<Vec<StoredRun>, HistoryError> {
        self.repository.list(&RunFilter { limit: limit.clamp(1, MAX_PAGE_SIZE), ..RunFilter::default() })
    }
}

pub struct GetRun {
    repository: Arc<dyn RunRepository>,
}

impl GetRun {
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self { repository }
    }

    pub fn execute(&self, id: i64) -> Result<Option<StoredRun>, HistoryError> {
        self.repository.get(id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub generated_at: DateTime<Utc>,
    pub last_7_days: PeriodStats,
    pub last_30_days: PeriodStats,
    pub repos: Vec<String>,
}

pub struct DashboardSummary {
    repository: Arc<dyn RunRepository>,
}

impl DashboardSummary {
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self { repository }
    }

    pub fn execute(&self, now: DateTime<Utc>) -> Result<Summary, HistoryError> {
        let long_since = now - Duration::days(LONG_PERIOD_DAYS);
        let runs = self.repository.since(long_since)?;
        Ok(Summary {
            generated_at: now,
            last_7_days: period_stats(&runs, now - Duration::days(SHORT_PERIOD_DAYS)),
            last_30_days: period_stats(&runs, long_since),
            repos: self.repository.repos()?,
        })
    }
}

/// Recurring findings over the last [`RECURRING_WINDOW_DAYS`] days.
pub struct RecurringIssues {
    repository: Arc<dyn RunRepository>,
}

impl RecurringIssues {
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self { repository }
    }

    pub fn execute(&self, now: DateTime<Utc>) -> Result<Vec<RecurringIssue>, HistoryError> {
        let runs = self.repository.since(now - Duration::days(RECURRING_WINDOW_DAYS))?;
        Ok(find_recurring(&runs, RECURRING_THRESHOLD))
    }
}

/// Every run concerning a ticket (its own key, or as parent Story of a reviewed Sub-task) and the
/// latest status of each of its acceptance criteria.
pub struct TicketHistory {
    repository: Arc<dyn RunRepository>,
}

impl TicketHistory {
    pub fn new(repository: Arc<dyn RunRepository>) -> Self {
        Self { repository }
    }

    pub fn execute(&self, raw_key: &str) -> Result<TicketProgress, HistoryError> {
        let key = TicketKey::parse(raw_key).map_err(|err| HistoryError::InvalidInput(err.to_string()))?;
        let runs: Vec<StoredRun> = self
            .repository
            .for_ticket(&key, MAX_TICKET_RUNS)?
            .into_iter()
            .filter(|run| mentions_ticket(run, &key))
            .collect();
        Ok(ticket_progress(&key, &runs))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::history::domain::fixtures::{at, finding, live_run, report_view, stored};
    use crate::review::application::{Category, CriterionStatus, Severity};

    /// In-memory port double; records the last filter it was asked for.
    #[derive(Default)]
    struct FakeRepo {
        runs: Vec<StoredRun>,
        inserted: Mutex<usize>,
        last_filter: Mutex<Option<RunFilter>>,
    }

    impl RunRepository for FakeRepo {
        fn insert(&self, _run: &ReviewRun) -> Result<i64, HistoryError> {
            let mut n = self.inserted.lock().unwrap();
            *n += 1;
            Ok(*n as i64)
        }
        fn get(&self, id: i64) -> Result<Option<StoredRun>, HistoryError> {
            Ok(self.runs.iter().find(|r| r.id == id).cloned())
        }
        fn list(&self, filter: &RunFilter) -> Result<Vec<StoredRun>, HistoryError> {
            *self.last_filter.lock().unwrap() = Some(filter.clone());
            Ok(self.runs.iter().skip(filter.offset as usize).take(filter.limit as usize).cloned().collect())
        }
        fn count(&self, _filter: &RunFilter) -> Result<u64, HistoryError> {
            Ok(self.runs.len() as u64)
        }
        fn since(&self, since: DateTime<Utc>) -> Result<Vec<StoredRun>, HistoryError> {
            Ok(self.runs.iter().filter(|r| r.created_at >= since).cloned().collect())
        }
        fn for_ticket(&self, _key: &TicketKey, limit: u32) -> Result<Vec<StoredRun>, HistoryError> {
            Ok(self.runs.iter().take(limit as usize).cloned().collect())
        }
        fn repos(&self) -> Result<Vec<String>, HistoryError> {
            Ok(vec!["api".into()])
        }
    }

    fn sample_runs() -> Vec<StoredRun> {
        let f = || finding(Severity::Major, Category::Security, "SQL built from `x`");
        let report = report_view(vec![f(), f()], &[CriterionStatus::Met, CriterionStatus::NotMet], 0);
        vec![
            stored(1, "api", at(19, 0), RunStatus::Passed, Some(report.clone())),
            stored(2, "api", at(1, 0), RunStatus::Failed, Some(report)),
            stored(3, "api", at(18, 0), RunStatus::Error, None),
        ]
    }

    fn repo(runs: Vec<StoredRun>) -> Arc<FakeRepo> {
        Arc::new(FakeRepo { runs, ..FakeRepo::default() })
    }

    #[test]
    fn record_run_validates_then_inserts() {
        let fake = repo(Vec::new());
        let record = RecordRun::new(fake.clone());
        assert_eq!(record.execute(&live_run("api", at(1, 0))).unwrap(), 1);

        let unnamed = ReviewRun { repo_name: " ".into(), ..live_run("api", at(1, 0)) };
        assert!(matches!(record.execute(&unnamed), Err(HistoryError::InvalidInput(_))));
        let silent_error = ReviewRun { status: RunStatus::Error, error: None, ..live_run("api", at(1, 0)) };
        assert!(matches!(record.execute(&silent_error), Err(HistoryError::InvalidInput(_))));
        assert_eq!(*fake.inserted.lock().unwrap(), 1);
    }

    #[test]
    fn list_runs_clamps_limit_and_maps_items() {
        let fake = repo(sample_runs());
        let list = ListRuns::new(fake.clone());

        let page = list.execute(Some(" api ".into()), Some(RunStatus::Passed), Some(10_000), 0).unwrap();
        let used = fake.last_filter.lock().unwrap().clone().unwrap();
        assert_eq!(used.limit, MAX_PAGE_SIZE);
        assert_eq!(used.repo.as_deref(), Some("api"));
        assert_eq!(used.status, Some(RunStatus::Passed));
        assert_eq!((page.total, page.limit, page.items.len()), (3, MAX_PAGE_SIZE, 3));
        assert_eq!(page.items[0].findings.major, 2);
        assert_eq!((page.items[0].criteria_met, page.items[0].criteria_total), (1, 2));
        assert_eq!(page.items[2].criteria_total, 0);

        let page = list.execute(Some(String::new()), None, Some(0), 1).unwrap();
        let used = fake.last_filter.lock().unwrap().clone().unwrap();
        assert_eq!((used.limit, used.offset, used.repo), (1, 1, None));
        assert_eq!(page.items[0].id, 2);
        list.execute(None, None, None, 0).unwrap();
        assert_eq!(fake.last_filter.lock().unwrap().as_ref().unwrap().limit, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn get_run_returns_none_for_unknown_id() {
        let get = GetRun::new(repo(sample_runs()));
        assert_eq!(get.execute(1).unwrap().unwrap().id, 1);
        assert!(get.execute(42).unwrap().is_none());
    }

    #[test]
    fn summary_splits_seven_and_thirty_day_windows() {
        let summary = DashboardSummary::new(repo(sample_runs())).execute(at(20, 0)).unwrap();
        assert_eq!(summary.last_7_days.runs, 2);
        assert_eq!(summary.last_7_days.pass_rate, Some(1.0));
        assert_eq!(summary.last_30_days.runs, 3);
        assert_eq!(summary.last_30_days.pass_rate, Some(0.5));
        assert_eq!(summary.repos, vec!["api".to_string()]);
    }

    #[test]
    fn recurring_issues_uses_threshold() {
        let issues = RecurringIssues::new(repo(sample_runs())).execute(at(20, 0)).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].occurrences, 4);
    }

    #[test]
    fn ticket_history_validates_the_key_and_drops_over_matches() {
        let mention = |id, key: &str| StoredRun {
            ticket_key: Some(key.into()),
            ..stored(id, "api", at(id as u32, 0), RunStatus::Passed, None)
        };
        let history = TicketHistory::new(repo(vec![mention(1, "SHOP-7"), mention(2, "SHOP-70"), mention(3, "SHOP-7")]));

        let progress = history.execute("SHOP-7").unwrap();
        assert_eq!(progress.runs.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 1]);
        for bad in ["shop-7", "SHOP-7' OR 1=1", "../x", ""] {
            assert!(matches!(history.execute(bad), Err(HistoryError::InvalidInput(_))), "{bad}");
        }
    }
}
