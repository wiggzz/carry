//! Coarse Pi-style counterfactual from request sizes, not message contents.
use crate::openai::{Usage, estimated_cost_usd};

const RESERVE: u64 = 16_384;
const KEEP_RECENT: u64 = 20_000;
const SUMMARY: u64 = 1_000;

#[derive(Clone)]
pub struct Request {
    pub model: String,
    pub usage: Usage,
    pub actual_cost: Option<f64>,
    pub dropped_before: u64,
}

#[derive(Default, Debug)]
pub struct Estimate {
    pub savings: f64,
    pub actual_cost: f64,
    pub pi_compactions: usize,
    pub summary_cost: f64,
    pub priced: bool,
}

/// Incrementally reconstruct request metadata for both web replay and offline reports.
#[derive(Default)]
pub struct Trajectory {
    pub requests: Vec<Request>,
    model: String,
    dropped: u64,
}

impl Trajectory {
    pub fn with_model(model: String) -> Self {
        Self {
            model,
            ..Self::default()
        }
    }

    pub fn record(&mut self, value: &serde_json::Value) -> Option<Estimate> {
        match value["event"].as_str()? {
            "run_started" | "session_resumed" => {
                if let Some(name) = value["data"]["model"].as_str() {
                    self.model = name.to_owned();
                }
            }
            "context_compacted" => {
                self.dropped = self.dropped.saturating_add(
                    value["data"]["compaction"]["dropped_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                );
            }
            "model_response" => {
                let usage = serde_json::from_value::<Usage>(value["data"]["usage"].clone()).ok()?;
                self.requests.push(Request {
                    model: self.model.clone(),
                    usage,
                    actual_cost: value["data"]["estimated_cost_usd"].as_f64(),
                    dropped_before: std::mem::take(&mut self.dropped),
                });
                return Some(estimate(&self.requests, 272_000));
            }
            _ => {}
        }
        None
    }
}

/// Savings as a percentage of the estimated alternative cost (not Carry's cost).
pub fn savings_percent(savings: f64, actual_cost: f64) -> Option<f64> {
    let alternative = actual_cost + savings;
    (alternative.is_finite() && alternative > 0.0).then_some(100.0 * savings / alternative)
}

/// Re-simulates cache reuse and Pi compactions; never imports Carry's cache classifications
/// into the alternative trajectory. No extrapolation of model outputs or tool activity.
pub fn estimate(requests: &[Request], context_window: u64) -> Estimate {
    let mut result = Estimate {
        priced: true,
        ..Estimate::default()
    };
    let mut carry_removed = 0u64;
    let mut pi_removed = 0u64;
    let mut pi_summary = 0u64;
    let mut prev_input = 0u64;
    let mut prev_output = 0u64;
    let mut prev_model = "";
    for request in requests {
        let Some(actual) = request
            .actual_cost
            .or_else(|| estimated_cost_usd(&request.model, &request.usage))
        else {
            result.priced = false;
            break;
        };
        result.actual_cost += actual;
        carry_removed = carry_removed.saturating_add(request.dropped_before);
        let raw = request.usage.input_tokens.saturating_add(carry_removed);
        let mut input = raw.saturating_sub(pi_removed).saturating_add(pi_summary);
        let mut compacted = false;
        // Pi checks after the preceding response, leaving room for the next output.
        if input.saturating_add(prev_output) > context_window.saturating_sub(RESERVE) {
            let removed = input.saturating_sub(KEEP_RECENT);
            let summary_usage = Usage {
                input_tokens: removed,
                output_tokens: SUMMARY,
                ..Usage::default()
            };
            let Some(cost) = estimated_cost_usd(&request.model, &summary_usage) else {
                result.priced = false;
                break;
            };
            result.summary_cost += cost;
            result.pi_compactions += 1;
            pi_removed = raw.saturating_sub(KEEP_RECENT);
            pi_summary = SUMMARY;
            input = raw.saturating_sub(pi_removed).saturating_add(pi_summary);
            compacted = true;
        }
        // After an edit the prefix is rewritten. Otherwise the previous prompt is
        // reusable, including any Carry-dropped material, and only growth is written.
        let cached = if compacted || prev_model != request.model {
            0
        } else {
            prev_input.min(input)
        };
        let hypothetical = Usage {
            input_tokens: input,
            cached_input_tokens: cached,
            cache_write_input_tokens: if compacted
                || !prev_model.is_empty() && prev_model == request.model
            {
                input - cached
            } else {
                0
            },
            output_tokens: request.usage.output_tokens,
            ..Usage::default()
        };
        let Some(cost) = estimated_cost_usd(&request.model, &hypothetical) else {
            result.priced = false;
            break;
        };
        result.savings += cost - actual;
        prev_input = input;
        prev_output = request.usage.output_tokens;
        prev_model = &request.model;
    }
    result.savings += result.summary_cost;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn req(input: u64, dropped: u64) -> Request {
        Request {
            model: "gpt-6-sol".into(),
            usage: Usage {
                input_tokens: input,
                output_tokens: 100,
                ..Usage::default()
            },
            actual_cost: None,
            dropped_before: dropped,
        }
    }
    #[test]
    fn no_compactions_and_carry_rewrites_are_not_reused_in_pi() {
        let mut second = req(1000, 600);
        second.usage.cache_write_input_tokens = 1000;
        let first = req(1000, 0);
        let e = estimate(&[first, second], 272_000);
        assert!(e.priced);
        assert_eq!(e.pi_compactions, 0);
        let expected = estimated_cost_usd(
            "gpt-6-sol",
            &Usage {
                input_tokens: 1600,
                cached_input_tokens: 1000,
                cache_write_input_tokens: 600,
                output_tokens: 100,
                ..Usage::default()
            },
        )
        .unwrap();
        assert!(
            (e.savings - expected
                + estimated_cost_usd(
                    "gpt-6-sol",
                    &Usage {
                        input_tokens: 1000,
                        cache_write_input_tokens: 1000,
                        output_tokens: 100,
                        ..Usage::default()
                    }
                )
                .unwrap())
            .abs()
                < 1e-9
        );
    }
    #[test]
    fn percentage_uses_alternative_cost_and_handles_zero() {
        assert_eq!(savings_percent(4.0, 1.0), Some(80.0));
        assert_eq!(savings_percent(0.0, 0.0), None);
        assert_eq!(savings_percent(-1.0, 1.0), None);
    }
    #[test]
    fn unchanged_trajectory_has_zero_savings() {
        let e = estimate(&[req(1000, 0)], 272_000);
        assert_eq!(e.savings, 0.0);
        assert_eq!(e.pi_compactions, 0);
    }
    #[test]
    fn repeats_compaction_and_charges_summary_and_rewrites() {
        let requests = [req(240_000, 0), req(260_000, 0), req(260_000, 245_000)];
        let e = estimate(&requests, 272_000);
        assert!(e.priced);
        assert_eq!(e.pi_compactions, 2);
        assert!(e.summary_cost > 0.9);
    }
    #[test]
    fn final_request_can_compact_even_without_payback() {
        let e = estimate(&[req(240_000, 0), req(260_000, 0)], 272_000);
        assert_eq!(e.pi_compactions, 1);
        assert!(e.summary_cost > 0.4);
    }
}
