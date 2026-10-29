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

pub use crate::models::Rates;

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

/// Independently observed input/cache pairs, not inferred from monetary pricing.
/// Legacy ledgers have no coverage; their historical numeric totals stay intact.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CacheObservation {
    pub calls: u64,
    pub input_tokens: u64,
    pub cached_tokens: u64,
}

/// Numeric native usage is kept separate from byte-estimated scenario sizes.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UsageLedger {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub cache_observation: CacheObservation,
    #[serde(default)]
    pub unpriced_models: std::collections::BTreeSet<String>,
    pub unavailable_cost_calls: u64,
    pub cost_usd: f64,
}

impl UsageLedger {
    pub fn observe(&mut self, model: &str, usage: &Value, standard: bool) {
        let rates = usage["input_tokens"]
            .as_u64()
            .and_then(|input| Rates::for_model(model, input as f64));
        self.observe_with_rates(model, usage, standard, rates);
    }

    /// Rates already selected from the dispatched frozen contract using native
    /// input count. None remains unknown: do not silently re-resolve the slug.
    pub fn observe_with_rates(
        &mut self,
        model: &str,
        usage: &Value,
        standard: bool,
        rates: Option<Rates>,
    ) {
        if rates.is_none() {
            self.unpriced_models.insert(model.to_owned());
        }
        let mut complete = self.calls.checked_add(1).is_some();
        self.calls = self.calls.saturating_add(1);
        let input = usage["input_tokens"].as_u64();
        let output = usage["output_tokens"].as_u64();
        let cached = usage["input_tokens_details"]["cached_tokens"].as_u64();
        if let (Some(input), Some(cached)) = (input, cached)
            && cached <= input
        {
            let observed = &mut self.cache_observation;
            if let (Some(calls), Some(input), Some(cached)) = (
                observed.calls.checked_add(1),
                observed.input_tokens.checked_add(input),
                observed.cached_tokens.checked_add(cached),
            ) {
                *observed = CacheObservation {
                    calls,
                    input_tokens: input,
                    cached_tokens: cached,
                };
            }
        }
        let written = usage["input_tokens_details"]["cache_write_tokens"]
            .as_u64()
            .or_else(|| usage["input_tokens_details"]["cache_creation_tokens"].as_u64())
            .unwrap_or(0);
        // Commit all token partitions together, or retain the prior receipt totals.
        if let (Some(input), Some(output), Some(cached), Some(written)) = (
            self.input_tokens.checked_add(input.unwrap_or(0)),
            self.output_tokens.checked_add(output.unwrap_or(0)),
            self.cached_tokens.checked_add(cached.unwrap_or(0)),
            self.cache_write_tokens.checked_add(written),
        ) {
            self.input_tokens = input;
            self.output_tokens = output;
            self.cached_tokens = cached;
            self.cache_write_tokens = written;
        } else {
            complete = false;
        }
        if let (Some(input), Some(output), Some(cached), Some(rates)) =
            (input, output, cached, rates)
            && standard
            && !["input_tokens_details", "output_tokens_details"]
                .iter()
                .any(|field| {
                    usage[*field]["audio_tokens"]
                        .as_u64()
                        .is_some_and(|n| n > 0)
                })
            && cached.checked_add(written).is_some_and(|v| v <= input)
        {
            // Token overflow does not erase an independently priceable receipt.
            let cost = (input - cached - written) as f64 * rates.input
                + cached as f64 * rates.cached
                + written as f64 * rates.write
                + output as f64 * rates.output;
            let total = self.cost_usd + cost;
            if cost.is_finite() && total.is_finite() {
                self.cost_usd = total;
            } else {
                complete = false;
            }
        } else {
            complete = false;
        }
        if !complete {
            self.unavailable_cost_calls = self.unavailable_cost_calls.saturating_add(1);
        }
    }
}
