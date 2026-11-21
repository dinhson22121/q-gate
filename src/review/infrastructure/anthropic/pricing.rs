//! Cost estimate of a review, priced by the shared table in [`crate::llm`].

use crate::llm::Usage;
use crate::review::domain::report::TokenUsage;

/// Estimated cost in USD of `usage` on `model`, or `None` when the model's price is unknown.
pub fn estimate_cost_usd(model: &str, usage: &TokenUsage) -> Option<f64> {
    let usage = Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
    };
    crate::llm::estimate_cost_usd(model, &usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, read: u64, write: u64) -> TokenUsage {
        TokenUsage { input_tokens: input, output_tokens: output, cache_read_tokens: read, cache_write_tokens: write }
    }

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("price should be known");
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn maps_every_token_kind_to_the_shared_table() {
        assert_close(estimate_cost_usd("claude-opus-5-5", &usage(1_000_000, 0, 0, 0)), 4.0);
        assert_close(estimate_cost_usd("claude-opus-5-5", &usage(0, 1_000_000, 0, 0)), 20.0);
        assert_close(estimate_cost_usd("claude-opus-5-5", &usage(0, 0, 1_000_000, 0)), 0.20);
        assert_close(estimate_cost_usd("claude-opus-5-5", &usage(0, 0, 0, 1_000_000)), 5.0);
    }

    #[test]
    fn unknown_model_has_no_price() {
        assert_eq!(estimate_cost_usd("gpt-9", &usage(1, 1, 1, 1)), None);
    }
}
