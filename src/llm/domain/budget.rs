//! Token usage and the spend tracker shared by all agents of one review.

use std::ops::AddAssign;
use std::sync::{Arc, Mutex, PoisonError};

use super::pricing::estimate_cost_usd;

/// Tokens consumed by one or more responses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(other.cache_write_tokens);
    }
}

#[derive(Debug, Default)]
struct Spend {
    usage: Usage,
    spent_usd: f64,
}

/// Shared spend tracker for one review (all agents). Cheap to clone: clones share the same state.
///
/// Tokens of models with an unknown price are counted but add no dollars, so they never exhaust it.
#[derive(Debug, Clone)]
pub struct Budget {
    limit_usd: f64,
    spend: Arc<Mutex<Spend>>,
}

impl Budget {
    /// Budget capped at `limit_usd`. A negative or NaN limit is treated as zero (nothing may be spent).
    pub fn new(limit_usd: f64) -> Self {
        let limit_usd = if limit_usd.is_nan() { 0.0 } else { limit_usd.max(0.0) };
        Self { limit_usd, spend: Arc::default() }
    }

    pub fn unlimited() -> Self {
        Self::new(f64::INFINITY)
    }

    pub fn limit_usd(&self) -> f64 {
        self.limit_usd
    }

    pub fn spent_usd(&self) -> f64 {
        self.lock().spent_usd
    }

    /// `limit - spent`; infinite for an unlimited budget, may go negative after the last response.
    pub fn remaining_usd(&self) -> f64 {
        self.limit_usd - self.spent_usd()
    }

    pub fn usage(&self) -> Usage {
        self.lock().usage
    }

    /// Adds one response's usage, priced for `model`. Returns that response's cost when the price is known.
    pub fn record(&self, model: &str, usage: &Usage) -> Option<f64> {
        let cost = estimate_cost_usd(model, usage);
        let mut spend = self.lock();
        spend.usage += *usage;
        spend.spent_usd += cost.unwrap_or(0.0);
        cost
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Spend> {
        // The guarded data is plain counters, always consistent, so a poisoned lock is still usable.
        self.spend.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MTOK: u64 = 1_000_000;

    #[test]
    fn records_tokens_and_dollars_across_clones() {
        let budget = Budget::new(10.0);
        let shared = budget.clone();
        let cost = shared.record("claude-opus-5-5", &Usage { input_tokens: MTOK, ..Usage::default() });

        assert_eq!(cost, Some(4.0));
        assert!((budget.spent_usd() - 4.0).abs() < 1e-9);
        assert!((budget.remaining_usd() - 6.0).abs() < 1e-9);
        assert_eq!(budget.usage().input_tokens, MTOK);
        assert!((budget.limit_usd() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_model_counts_tokens_but_not_dollars() {
        let budget = Budget::new(1.0);
        let usage = Usage { input_tokens: 1, output_tokens: 2, cache_read_tokens: 3, cache_write_tokens: 4 };

        assert_eq!(budget.record("mystery-model", &usage), None);
        budget.record("mystery-model", &usage);

        assert_eq!(
            budget.usage(),
            Usage { input_tokens: 2, output_tokens: 4, cache_read_tokens: 6, cache_write_tokens: 8 }
        );
        assert!(budget.spent_usd().abs() < f64::EPSILON);
    }

    #[test]
    fn unlimited_never_runs_out() {
        let budget = Budget::unlimited();
        budget.record("claude-fable-5-2", &Usage { output_tokens: 100 * MTOK, ..Usage::default() });
        assert!(budget.remaining_usd().is_infinite());
    }

    #[test]
    fn negative_or_nan_limit_means_nothing_may_be_spent() {
        assert!(Budget::new(-1.0).remaining_usd() <= 0.0);
        assert!(Budget::new(f64::NAN).remaining_usd() <= 0.0);
    }
}
