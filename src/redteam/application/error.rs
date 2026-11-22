#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RedteamError {
    #[error("target host is not in the redteam allowlist: {0}")]
    HostNotAllowed(String),
    #[error("target.base_url is not a valid absolute URL")]
    InvalidTarget,
    #[error("http probe failed: {0}")]
    Probe(String),
    #[error("attack model failed: {0}")]
    Model(String),
    #[error("Claude usage limit reached; try again later")]
    RateLimited,
    #[error("storage error: {0}")]
    Storage(String),
    #[error("scan report not found")]
    NotFound,
}
