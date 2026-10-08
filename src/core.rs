use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Both frontends supply semantic eligibility and atomic IDs. This is the sole
/// ordered removal selection; codecs must never split one selected unit.
pub fn select_removals(
    ordered_ids: impl IntoIterator<Item = u64>,
    eligible: &HashSet<u64>,
    protected: &HashSet<u64>,
) -> Vec<u64> {
    ordered_ids
        .into_iter()
        .filter(|id| eligible.contains(id) && !protected.contains(id))
        .collect()
}

/// Prefix reuse is evidence, not similarity. Never reuse across a rewrite.
pub fn prefix_compatible<T: PartialEq>(input: &[T], prefix: &[T]) -> bool {
    input.starts_with(prefix)
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Rates {
    pub input: f64,
    pub cached: f64,
    pub write: f64,
    pub output: f64,
}

impl Rates {
    /// Verified standard rates; exact names only, no inferred neighbor aliases.
    /// Full-request long-context pricing is strictly above 272,000 input tokens.
    pub fn for_model(model: &str, input_tokens: f64) -> Option<Self> {
        let (input, cached, write, output) = match model {
            "gpt-6-luna" => (0.10, 0.01, 0.125, 0.50),
            "gpt-6-sol" => (2.00, 0.20, 2.50, 10.00),
            "gpt-6.1-sol" => (2.00, 0.10, 2.50, 10.00),
            _ => return None,
        };
        let long = input_tokens > 272_000.0;
        Some(Self {
            input: input * if long { 2.0 } else { 1.0 } / 1_000_000.0,
            cached: cached * if long { 2.0 } else { 1.0 } / 1_000_000.0,
            write: write * if long { 2.0 } else { 1.0 } / 1_000_000.0,
            output: output * if long { 1.5 } else { 1.0 } / 1_000_000.0,
        })
    }
}

pub fn input_cost(tokens: f64, cached: f64, uncached_rate: f64, cached_rate: f64) -> f64 {
    let cached = cached.clamp(0.0, tokens);
    (tokens - cached) * uncached_rate + cached * cached_rate
}

/// Fixed-history scenario, not a prediction of future growth. The first cost is
/// independently priced; later calls use the explicitly supplied future rate.
pub fn horizon_cost(first: f64, tokens: f64, future_rate: f64, requests: u64) -> f64 {
    if requests == 0 {
        return 0.0;
    }
    first + requests.saturating_sub(1) as f64 * tokens * future_rate
}

#[derive(Clone, Copy, Debug)]
pub struct ViewCosts {
    pub keep: f64,
    pub compact: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct JointDecision {
    pub primary_keep: f64,
    pub primary_compact: f64,
    pub future_shadow_keep: f64,
    pub future_shadow_compact: f64,
    pub keep: f64,
    pub compact: f64,
    pub savings: f64,
    pub accepted: bool,
}

/// Current completed review is sunk in BOTH alternatives, so is deliberately
/// not an argument. Future reviewer cadence is priced separately by the caller.
pub fn joint_decision(primary: ViewCosts, shadow: ViewCosts, min_percent: u8) -> JointDecision {
    let keep = primary.keep + shadow.keep;
    let compact = primary.compact + shadow.compact;
    let savings = keep - compact;
    JointDecision {
        primary_keep: primary.keep,
        primary_compact: primary.compact,
        future_shadow_keep: shadow.keep,
        future_shadow_compact: shadow.compact,
        keep,
        compact,
        savings,
        accepted: keep.is_finite()
            && compact.is_finite()
            && savings > keep * f64::from(min_percent) / 100.0,
    }
}

/// Numeric native usage is kept separate from byte-estimated scenario sizes.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UsageLedger {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub unavailable_cost_calls: u64,
    pub cost_usd: f64,
}

impl UsageLedger {
    pub fn observe(&mut self, model: &str, usage: &Value, standard: bool) {
        self.calls += 1;
        let input = usage["input_tokens"].as_u64();
        let output = usage["output_tokens"].as_u64();
        let cached = usage["input_tokens_details"]["cached_tokens"].as_u64();
        let written = usage["input_tokens_details"]["cache_write_tokens"]
            .as_u64()
            .or_else(|| usage["input_tokens_details"]["cache_creation_tokens"].as_u64())
            .unwrap_or(0);
        self.input_tokens += input.unwrap_or(0);
        self.output_tokens += output.unwrap_or(0);
        self.cached_tokens += cached.unwrap_or(0);
        self.cache_write_tokens += written;
        if let (Some(input), Some(output), Some(cached), Some(rates)) = (
            input,
            output,
            cached,
            input.and_then(|i| Rates::for_model(model, i as f64)),
        )
            && standard
            && cached.checked_add(written).is_some_and(|v| v <= input)
        {
            self.cost_usd += (input - cached - written) as f64 * rates.input
                + cached as f64 * rates.cached
                + written as f64 * rates.write
                + output as f64 * rates.output;
        } else {
            self.unavailable_cost_calls += 1;
        }
    }
}
