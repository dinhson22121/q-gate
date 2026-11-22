use futures::future::BoxFuture;

use super::error::RedteamError;
use crate::redteam::domain::{AttackSurface, Endpoint, Hypothesis, ProbeRequest, ProbeResponse, ScanReport};

pub trait AttackModel: Send + Sync {
    fn propose_attacks<'a>(
        &'a self,
        endpoint: &'a Endpoint,
        surface: &'a AttackSurface,
    ) -> BoxFuture<'a, Result<Vec<Hypothesis>, RedteamError>>;

    fn preflight(&self) -> BoxFuture<'_, Result<(), RedteamError>> {
        Box::pin(async { Ok(()) })
    }
}

pub trait HttpProbe: Send + Sync {
    fn send<'a>(&'a self, request: &'a ProbeRequest) -> BoxFuture<'a, Result<ProbeResponse, RedteamError>>;
}

pub trait TargetPolicy: Send + Sync {
    fn check_host(&self, host: &str) -> Result<(), RedteamError>;
    fn allows_destructive(&self, host: &str) -> bool;
}

pub trait ScanStore: Send + Sync {
    fn save(&self, report: &ScanReport) -> Result<i64, RedteamError>;
    fn get(&self, id: i64) -> Result<Option<ScanReport>, RedteamError>;
    fn list(&self, limit: u32, offset: u32) -> Result<Vec<ScanReport>, RedteamError>;
}
