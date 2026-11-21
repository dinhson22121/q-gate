//! Published Claude list prices: the single price table of qgate.

use super::budget::Usage;

const TOKENS_PER_MILLION: f64 = 1_000_000.0;

/// USD per million tokens for one model.
struct Price {
    model: &'static str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

const PRICES: [Price; 6] = [
    Price { model: "claude-opus-5-5", input: 4.0, output: 20.0, cache_read: 0.20, cache_write: 5.0 },
    Price { model: "claude-opus-5", input: 5.0, output: 25.0, cache_read: 0.50, cache_write: 6.25 },
    Price { model: "claude-opus-4-8", input: 5.0, output: 25.0, cache_read: 0.50, cache_write: 6.25 },
    Price { model: "claude-sonnet-5-5", input: 2.0, output: 10.0, cache_read: 0.10, cache_write: 2.5 },
    Price { model: "claude-fable-5-2", input: 8.0, output: 40.0, cache_read: 0.20, cache_write: 10.0 },
    Price { model: "claude-haiku-5-5", input: 0.10, output: 0.50, cache_read: 0.01, cache_write: 0.125 },
];

/// Estimated cost in USD of `usage` on `model`, or `None` when the model's price is unknown.
///
/// Matches the exact model id first, then the longest known id that prefixes a dated
/// variant (e.g. `claude-opus-5-5-20260101`).
pub fn estimate_cost_usd(model: &str, usage: &Usage) -> Option<f64> {
    let price = find_price(model)?;
    let total = per_million(usage.input_tokens, price.input)
        + per_million(usage.output_tokens, price.output)
        + per_million(usage.cache_read_tokens, price.cache_read)
        + per_million(usage.cache_write_tokens, price.cache_write);
    Some(total)
}

fn find_price(model: &str) -> Option<&'static Price> {
    let model = model.trim();
    PRICES.iter().find(|p| p.model == model).or_else(|| {
        PRICES
            .iter()
            .filter(|p| model.strip_prefix(p.model).is_some_and(|rest| rest.starts_with('-')))
            .max_by_key(|p| p.model.len())
    })
}

#[allow(clippy::cast_precision_loss)] // token counts are far below 2^52
fn per_million(tokens: u64, usd_per_million: f64) -> f64 {
    tokens as f64 * usd_per_million / TOKENS_PER_MILLION
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, read: u64, write: u64) -> Usage {
        Usage { input_tokens: input, output_tokens: output, cache_read_tokens: read, cache_write_tokens: write }
    }

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("price should be known");
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn prices_all_token_kinds_for_default_model() {
        let cost = estimate_cost_usd("claude-opus-5-5", &usage(1_000_000, 1_000_000, 1_000_000, 1_000_000));
        assert_close(cost, 4.0 + 20.0 + 0.20 + 5.0);
    }

    #[test]
    fn distinguishes_opus_5_from_opus_5_5() {
        assert_close(estimate_cost_usd("claude-opus-5", &usage(1_000_000, 0, 0, 0)), 5.0);
        assert_close(estimate_cost_usd("claude-haiku-5-5", &usage(0, 2_000_000, 0, 0)), 1.0);
        assert_close(estimate_cost_usd("claude-fable-5-2", &usage(0, 0, 0, 1_000_000)), 10.0);
        assert_close(estimate_cost_usd("claude-sonnet-5-5", &usage(500_000, 0, 0, 0)), 1.0);
        assert_close(estimate_cost_usd("claude-opus-4-8", &usage(0, 0, 2_000_000, 0)), 1.0);
    }

    #[test]
    fn dated_variant_uses_longest_matching_id() {
        assert_close(estimate_cost_usd("claude-opus-5-5-20260101", &usage(1_000_000, 0, 0, 0)), 4.0);
    }

    #[test]
    fn unknown_model_has_no_price() {
        assert_eq!(estimate_cost_usd("gpt-9", &usage(1, 1, 1, 1)), None);
        assert_eq!(estimate_cost_usd("claude-opus-55", &usage(1, 1, 1, 1)), None);
    }
}
