use std::sync::Arc;

use super::error::RedteamError;
use super::ports::ScanStore;
use super::run_pentest::RunPentest;
use crate::redteam::domain::{ScanReport, ScanTarget};

pub struct RedteamService {
    run_pentest: RunPentest,
    store: Arc<dyn ScanStore>,
}

impl RedteamService {
    pub fn new(run_pentest: RunPentest, store: Arc<dyn ScanStore>) -> Self {
        Self { run_pentest, store }
    }

    pub async fn scan(&self, target: ScanTarget) -> Result<ScanReport, RedteamError> {
        let mut report = self.run_pentest.execute(target).await?;
        let id = self.store.save(&report)?;
        report.id = Some(id);
        Ok(report)
    }

    pub fn get(&self, id: i64) -> Result<ScanReport, RedteamError> {
        self.store.get(id)?.ok_or(RedteamError::NotFound)
    }

    pub fn list(&self, limit: u32, offset: u32) -> Result<Vec<ScanReport>, RedteamError> {
        self.store.list(limit, offset)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::future::BoxFuture;

    use super::super::ports::{AttackModel, HttpProbe, TargetPolicy};
    use super::*;
    use crate::redteam::domain::{AttackSurface, Endpoint, Hypothesis, ProbeRequest, ProbeResponse, ScanBudget};

    struct AllowAll;
    impl TargetPolicy for AllowAll {
        fn check_host(&self, _host: &str) -> Result<(), RedteamError> {
            Ok(())
        }
        fn allows_destructive(&self, _host: &str) -> bool {
            false
        }
    }

    struct NoHypotheses;
    impl AttackModel for NoHypotheses {
        fn propose_attacks<'a>(
            &'a self,
            _endpoint: &'a Endpoint,
            _surface: &'a AttackSurface,
        ) -> BoxFuture<'a, Result<Vec<Hypothesis>, RedteamError>> {
            Box::pin(async { Ok(vec![]) })
        }
    }

    struct DeadProbe;
    impl HttpProbe for DeadProbe {
        fn send<'a>(&'a self, _request: &'a ProbeRequest) -> BoxFuture<'a, Result<ProbeResponse, RedteamError>> {
            Box::pin(async { Err(RedteamError::Probe("down".into())) })
        }
    }

    struct MemoryStore {
        reports: Mutex<Vec<ScanReport>>,
    }

    impl ScanStore for MemoryStore {
        fn save(&self, report: &ScanReport) -> Result<i64, RedteamError> {
            let mut reports = self.reports.lock().unwrap();
            reports.push(report.clone());
            Ok(reports.len() as i64)
        }
        fn get(&self, id: i64) -> Result<Option<ScanReport>, RedteamError> {
            Ok(self.reports.lock().unwrap().get((id - 1) as usize).cloned())
        }
        fn list(&self, limit: u32, offset: u32) -> Result<Vec<ScanReport>, RedteamError> {
            let reports = self.reports.lock().unwrap();
            Ok(reports.iter().skip(offset as usize).take(limit as usize).cloned().collect())
        }
    }

    fn service() -> RedteamService {
        let run_pentest =
            RunPentest::new(Arc::new(NoHypotheses), Arc::new(DeadProbe), Arc::new(AllowAll), ScanBudget::default());
        RedteamService::new(run_pentest, Arc::new(MemoryStore { reports: Mutex::new(Vec::new()) }))
    }

    #[tokio::test]
    async fn a_scan_is_saved_and_can_be_fetched_by_id() {
        let service = service();
        let report =
            service.scan(ScanTarget { base_url: "http://127.0.0.1".into(), ..ScanTarget::default() }).await.unwrap();
        let id = report.id.expect("saved report has an id");
        let fetched = service.get(id).unwrap();
        assert_eq!(fetched.target.base_url, "http://127.0.0.1");
        assert_eq!(service.list(10, 0).unwrap().len(), 1);
    }

    #[test]
    fn fetching_an_unknown_id_is_not_found() {
        let service = service();
        assert_eq!(service.get(99), Err(RedteamError::NotFound));
    }
}
