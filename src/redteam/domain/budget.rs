use std::time::Duration;

const DEFAULT_MAX_MODEL_CALLS: u32 = 40;
const DEFAULT_MAX_HTTP_REQUESTS: u32 = 200;
const DEFAULT_WALL_CLOCK: Duration = Duration::from_secs(10 * 60);
const DEFAULT_MAX_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanBudget {
    pub max_model_calls: u32,
    pub max_http_requests: u32,
    pub wall_clock: Duration,
    pub max_concurrency: usize,
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self {
            max_model_calls: DEFAULT_MAX_MODEL_CALLS,
            max_http_requests: DEFAULT_MAX_HTTP_REQUESTS,
            wall_clock: DEFAULT_WALL_CLOCK,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
        }
    }
}
