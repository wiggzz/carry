//! Conditional frozen-trajectory scenario using local o200k_base token estimates.
//! Provider usage remains a separate modeled ledger.
use crate::openai::{Usage, estimated_cost_usd, prompt_cache_capabilities};
use carry::models::{ScenarioCacheMode, ScenarioCachePolicy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Scenario {
    pub context_window: u64,
    pub reserve: u64,
    pub keep_recent: u64,
    pub summary: u64,
}
impl Scenario {
    pub(crate) fn valid(&self) -> bool {
        self.context_window > self.reserve
            && self.keep_recent.saturating_add(self.summary) <= self.context_window - self.reserve
    }
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            context_window: u64::from(carry::models::default_ordinary_context_tokens()),
            reserve: 16_384,
            keep_recent: 20_000,
            summary: 1_000,
        }
    }
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct Coverage {
    pub requests: usize,
    pub responses: usize,
    pub unmatched_requests: usize,
    pub orphan_responses: usize,
    pub missing_usage: usize,
    pub missing_projection: usize,
    pub missing_output_projection: usize,
    pub unpriced_responses: usize,
    pub retries: u64,
    pub invalid_lines: usize,
    pub unknown_cache_requests: usize,
    pub unknown_expiry_requests: usize,
    pub unknown_route_requests: usize,
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct Estimate {
    pub counterfactual_cache_policy: String,
    pub savings: f64,
    pub observed_cost: f64,
    pub carry_cost: f64,
    pub pi_cost: f64,
    pub pi_compactions: usize,
    /// Free pre-window summaries, never included in charged compactions.
    pub initial_compactions: usize,
    pub initialization_policy: &'static str,
    pub summary_cost: f64,
    pub priced: bool,
    pub observed_priced: bool,
    pub recorded_window: bool,
    pub coverage: Coverage,
}

struct Prompt {
    model: String,
    // Only the previous prompt-bearing prefix survives; never a history array.
    prefix: String,
    overhead: String,
    route: String,
    input: u64,
    protected_input: u64,
    cached_at_ms: Option<u64>,
    cache_mode: ScenarioCacheMode,
    write_tariff_known: bool,
}

pub struct Trajectory {
    pub model: String,
    pub scenario: Scenario,
    configured_window: u64,
    result: Estimate,
    pending: Option<Prompt>,
    pending_request: bool,
    pending_step: Option<u64>,
    pending_run: Option<String>,
    previous: Option<Prompt>,
    restored: i128,
    removed: u64,
    summary: u64,
    last_pi_input: u64,
    last_output_proxy: u64,
    last_model: String,
    scenario_available: bool,
    initialized: bool,
    carry_edited: bool,
    pi_edited: bool,
    cache_ttl_ms: Option<u64>,
    cache_minimum: Option<u64>,
    cache_metadata: bool,
}
impl Default for Trajectory {
    fn default() -> Self {
        Self::with_scenario(Scenario::default())
    }
}
impl Trajectory {
    pub fn with_model(model: String) -> Self {
        Self {
            model,
            ..Self::default()
        }
    }
    pub fn with_scenario(scenario: Scenario) -> Self {
        Self {
            model: String::new(),
            configured_window: scenario.context_window,
            scenario,
            result: Estimate::default(),
            pending: None,
            pending_request: false,
            pending_step: None,
            pending_run: None,
            previous: None,
            restored: 0,
            removed: 0,
            summary: 0,
            last_pi_input: 0,
            last_output_proxy: 0,
            last_model: String::new(),
            scenario_available: true,
            initialized: false,
            carry_edited: false,
            pi_edited: false,
            cache_ttl_ms: None,
            cache_minimum: None,
            cache_metadata: false,
        }
    }
    pub fn snapshot(&self) -> Estimate {
        let mut e = self.result.clone();
        e.initialization_policy = "unrecognized_history_free_pre_window_v1";
        e.counterfactual_cache_policy =
            format!("conditional_model_cache_v{}", ScenarioCachePolicy::VERSION);
        e.coverage.unmatched_requests += usize::from(self.pending_request);
        e.observed_priced = e.coverage.responses > 0
            && e.coverage.unpriced_responses == 0
            && e.coverage.unmatched_requests == 0
            && e.coverage.missing_usage == 0
            && e.coverage.orphan_responses == 0
            && e.coverage.retries == 0
            && e.coverage.invalid_lines == 0;
        let model = if self.model.is_empty() {
            &self.last_model
        } else {
            &self.model
        };
        e.priced = self.scenario_available
            && self.scenario.valid()
            && e.observed_priced
            && e.coverage.unknown_cache_requests == 0
            && estimated_cost_usd(model, &Usage::default()).is_some();
        e.savings = e.pi_cost - e.carry_cost;
        e
    }
    pub fn invalid_line(&mut self) {
        self.result.coverage.invalid_lines += 1;
        self.scenario_available = false;
    }
    fn cache_minimum_for(&self, model: &str) -> Option<u64> {
        if self.cache_metadata && model == self.model {
            self.cache_minimum
        } else {
            prompt_cache_capabilities(model).map(|c| c.minimum_prefix_tokens as u64)
        }
    }
    pub fn record(&mut self, value: &Value) -> Option<Estimate> {
        let data = &value["data"];
        match value["event"].as_str()? {
            "run_started" | "session_resumed" => {
                let previous_minimum = self.cache_minimum_for(&self.last_model);
                if let Some(model) = data["model"].as_str() {
                    self.model = model.to_owned();
                }
                self.cache_ttl_ms = data["cache_ttl_seconds"]
                    .as_u64()
                    .map(|s| s.saturating_mul(1000));
                self.cache_metadata = data.get("implicit_cache_minimum_prefix_tokens").is_some();
                self.cache_minimum = data["implicit_cache_minimum_prefix_tokens"].as_u64();
                if previous_minimum != self.cache_minimum_for(&self.last_model) {
                    // A prior prompt was not necessarily written under the new policy.
                    self.previous = None;
                }
                let window = data["context_window"].as_u64().filter(|w| *w > 0);
                self.scenario.context_window = window.unwrap_or(self.configured_window);
                self.result.recorded_window = window.is_some();
            }
            "context_compacted" => {
                self.carry_edited = true;
                if let Some(dropped) = data["compaction_token_delta"].as_i64() {
                    self.restored = self.restored.saturating_add(dropped as i128);
                } else {
                    self.result.coverage.missing_projection += 1;
                    self.scenario_available = false;
                }
            }
            "cache_expired" => {
                self.previous = None;
            }
            "model_request" => {
                if self.pending_request {
                    self.result.coverage.unmatched_requests += 1;
                }
                self.pending_request = true;
                self.pending_step = data["step"].as_u64();
                self.pending_run = value["run_id"].as_str().map(str::to_owned);
                self.result.coverage.requests += 1;
                self.pending = projection(data, &self.model);
                if let Some(p) = self.pending.as_mut() {
                    p.cached_at_ms = value["timestamp_ms"].as_u64();
                }
                if self.cache_ttl_ms.is_none() || value["timestamp_ms"].as_u64().is_none() {
                    self.result.coverage.unknown_expiry_requests += 1;
                }
                if data["transport"].get("api_base").is_none() && data.get("route").is_none() {
                    self.result.coverage.unknown_route_requests += 1;
                }
            }
            "model_response" => {
                self.result.coverage.responses += 1;
                self.result.coverage.retries += data["response_retries"].as_u64().unwrap_or(0);
                let matched = self.pending_request
                    && self
                        .pending_step
                        .zip(data["step"].as_u64())
                        .is_none_or(|(a, b)| a == b)
                    && self
                        .pending_run
                        .as_deref()
                        .zip(value["run_id"].as_str())
                        .is_none_or(|(a, b)| a == b);
                if matched {
                    self.pending_request = false;
                } else {
                    self.result.coverage.orphan_responses += 1;
                    self.scenario_available = false;
                }
                let usage = valid_usage(&data["usage"]);
                if usage.is_none() {
                    self.result.coverage.missing_usage += 1;
                }
                if self.pending.is_none() {
                    self.result.coverage.missing_projection += 1;
                }
                let model = self.pending.as_ref().map_or(&self.model, |p| &p.model);
                if let Some(cost) = usage.as_ref().and_then(|u| {
                    (u.cache_write_input_tokens == 0
                        || self.pending.as_ref().is_some_and(|p| p.write_tariff_known))
                    .then(|| estimated_cost_usd(model, u))
                    .flatten()
                }) {
                    self.result.observed_cost += cost;
                } else {
                    self.result.coverage.unpriced_responses += 1;
                }
                let prompt = if matched { self.pending.take() } else { None };
                if let (Some(mut prompt), Some(usage)) = (prompt, usage) {
                    let mut pi_input = (prompt.input as i128)
                        .saturating_add(self.restored)
                        .saturating_sub(self.removed as i128)
                        .saturating_add(self.summary as i128)
                        .clamp(0, u64::MAX as i128) as u64;
                    // Initialize before pricing the first compared primary.
                    // Cache expiry does not erase known traditional history.
                    if !self.initialized && self.scenario.valid() {
                        let budget = self.scenario.context_window - self.scenario.reserve;
                        if pi_input > budget {
                            let kept = self.scenario.keep_recent.max(prompt.protected_input);
                            let compacted = kept.saturating_add(self.scenario.summary);
                            if compacted > budget || compacted >= pi_input {
                                self.scenario_available = false;
                            } else {
                                self.removed = self.removed.saturating_add(
                                    pi_input.saturating_sub(kept).saturating_sub(self.summary),
                                );
                                self.summary = self.scenario.summary;
                                pi_input = compacted;
                                self.pi_edited = true;
                                self.result.initial_compactions += 1;
                            }
                        }
                        self.initialized = true;
                    }
                    // Freeze the output count, not its rate: use each scenario input tier.
                    let carry_output =
                        scenario_output_cost(&prompt.model, prompt.input, usage.output_tokens);
                    let pi_output =
                        scenario_output_cost(&prompt.model, pi_input, usage.output_tokens);
                    let compatible = self.previous.as_ref().is_some_and(|p| {
                        p.model == prompt.model
                            && p.overhead == prompt.overhead
                            && p.route == prompt.route
                            && p.cache_mode == prompt.cache_mode
                            && self
                                .cache_ttl_ms
                                .zip(p.cached_at_ms)
                                .zip(prompt.cached_at_ms)
                                .is_none_or(|((ttl, prev), now)| now >= prev && now - prev < ttl)
                    });
                    let append = self
                        .previous
                        .as_ref()
                        .is_some_and(|p| prompt.prefix.starts_with(&p.prefix));
                    let carry_cached = if compatible && append && !self.carry_edited {
                        self.previous.as_ref().unwrap().input.min(prompt.input)
                    } else {
                        0
                    };
                    // Alternative prefixes are unknown after a native edit: declared append/frozen-size reuse.
                    let pi_cached =
                        if compatible && (append || self.carry_edited) && !self.pi_edited {
                            self.last_pi_input.min(pi_input)
                        } else {
                            0
                        };
                    let minimum = self.cache_minimum_for(&prompt.model);
                    if minimum.is_none() || prompt.cache_mode == ScenarioCacheMode::Unknown {
                        self.result.coverage.unknown_cache_requests += 1;
                    }
                    let carry = scenario_input_cost_with_mode(
                        &prompt.model,
                        prompt.input,
                        carry_cached,
                        minimum,
                        prompt.cache_mode,
                        estimated_cost_usd,
                    );
                    let pi = scenario_input_cost_with_mode(
                        &prompt.model,
                        pi_input,
                        pi_cached,
                        minimum,
                        prompt.cache_mode,
                        estimated_cost_usd,
                    );
                    if let (Some(carry), Some(pi), Some(carry_output), Some(pi_output)) =
                        (carry, pi, carry_output, pi_output)
                    {
                        self.result.carry_cost += carry + carry_output;
                        self.result.pi_cost += pi + pi_output;
                    } else {
                        self.scenario_available = false;
                    }
                    self.last_pi_input = pi_input;
                    if let Some(output) = data["raw"].get("output").filter(|v| !v.is_null()) {
                        self.last_output_proxy = token_units(output);
                    } else {
                        self.scenario_available = false;
                        self.result.coverage.missing_output_projection += 1;
                    }
                    self.last_model = prompt.model.clone();
                    prompt.cached_at_ms = value["timestamp_ms"].as_u64().or(prompt.cached_at_ms);
                    self.previous = Some(prompt);
                    self.carry_edited = false;
                    self.pi_edited = false;
                } else {
                    self.scenario_available = false;
                }
                // Check after each response, not just after the whole agent run.
                let total = self.last_pi_input.saturating_add(self.last_output_proxy);
                if self.scenario_available
                    && self.scenario.valid()
                    && !self.last_model.is_empty()
                    && total
                        > self
                            .scenario
                            .context_window
                            .saturating_sub(self.scenario.reserve)
                {
                    let discarded = total.saturating_sub(self.scenario.keep_recent);
                    let summary_usage = Usage {
                        input_tokens: discarded,
                        output_tokens: self.scenario.summary,
                        ..Usage::default()
                    };
                    if let Some(cost) = estimated_cost_usd(&self.last_model, &summary_usage) {
                        self.result.summary_cost += cost;
                        self.result.pi_cost += cost;
                        self.result.pi_compactions += 1;
                        self.pi_edited = true;
                        self.removed = self
                            .removed
                            .saturating_add(discarded.saturating_sub(self.summary));
                        self.summary = self.scenario.summary;
                        self.last_pi_input = self.scenario.keep_recent.saturating_add(self.summary);
                        self.last_output_proxy = 0;
                    } else {
                        self.scenario_available = false;
                    }
                }
            }
            // Completion notifications must not charge the last summary twice.
            "turn_finished" | "run_finished" => {}
            _ => return None,
        }
        Some(self.snapshot())
    }
}
#[cfg(test)]
fn input_cost(model: &str, input: u64, cached: u64, minimum: Option<u64>) -> Option<f64> {
    scenario_input_cost(model, input, cached, minimum, estimated_cost_usd)
}

/// Input selects the authoritative output tier but is not charged a second time.
pub(crate) fn scenario_output_cost(model: &str, input: u64, output: u64) -> Option<f64> {
    carry::models::Rates::for_model(model, input as f64).map(|rates| output as f64 * rates.output)
}

/// Legacy/default implicit policy; request-aware consumers use the mode owner.
pub(crate) fn scenario_input_cost(
    model: &str,
    input: u64,
    cached: u64,
    minimum: Option<u64>,
    price: impl FnOnce(&str, &Usage) -> Option<f64>,
) -> Option<f64> {
    scenario_input_cost_with_mode(
        model,
        input,
        cached,
        minimum,
        ScenarioCacheMode::Implicit,
        price,
    )
}

/// Shared request-population and eligibility math, never native receipt pricing.
pub(crate) fn scenario_input_cost_with_mode(
    model: &str,
    input: u64,
    cached: u64,
    minimum: Option<u64>,
    mode: ScenarioCacheMode,
    price: impl FnOnce(&str, &Usage) -> Option<f64>,
) -> Option<f64> {
    if mode == ScenarioCacheMode::Unknown {
        return None;
    }
    let minimum = if mode == ScenarioCacheMode::Disabled {
        None
    } else {
        minimum
    };
    let eligible = minimum.is_some_and(|min| input >= min);
    let cached = if minimum.is_some_and(|min| cached >= min) {
        cached.min(input)
    } else {
        0
    };
    price(
        model,
        &Usage {
            input_tokens: input,
            cached_input_tokens: cached,
            cache_write_input_tokens: if eligible { input - cached } else { 0 },
            ..Usage::default()
        },
    )
}
fn valid_usage(value: &Value) -> Option<Usage> {
    value["input_tokens"].as_u64()?;
    value["output_tokens"].as_u64()?;
    serde_json::from_value(value.clone()).ok()
}
pub(crate) fn token_units(value: &Value) -> u64 {
    // Rendered JSON is a local text estimate, not provider or multimodal tokenization.
    tiktoken_rs::o200k_base_singleton()
        .encode_ordinary(&value.to_string())
        .len() as u64
}

/// Shared prompt-bearing projection, excluding transport and sampling fields.
pub(crate) fn prompt_projection(body: &Value) -> Option<(String, String, u64)> {
    let input = body["input"].as_array()?;
    let overhead = json!({"instructions":body.get("instructions"),"tools":body.get("tools"),"text":body.get("text")});
    let units = token_units(&json!({"overhead":overhead,"input":input}));
    let overhead = overhead.to_string();
    let serialized = serde_json::to_string(input).ok()?;
    let prefix = format!("{overhead}{}", &serialized[..serialized.len() - 1]);
    Some((prefix, overhead, units))
}
/// Initialization may retire history, never fixed overhead or the current turn.
/// Keep the same local prompt units as `prompt_projection` in both estimators.
pub(crate) fn protected_prompt_tokens(body: &Value) -> Option<u64> {
    let input = body["input"].as_array()?;
    let current = input.iter().rposition(|item| item["role"] == "user");
    let protected = current.map_or(&input[input.len()..], |start| &input[start..]);
    let overhead = json!({"instructions":body.get("instructions"),"tools":body.get("tools"),"text":body.get("text")});
    Some(token_units(&json!({"overhead":overhead,"input":protected})))
}

fn projection(data: &Value, fallback: &str) -> Option<Prompt> {
    let request = &data["request"];
    let (prefix, overhead, input) = prompt_projection(request)?;
    let protected_input = protected_prompt_tokens(request)?;
    // Semantic policy is compared separately; equivalent default spellings do
    // not change the real route/key or force an artificial cold request.
    let route=serde_json::to_string(&json!({"api_base":data["transport"].get("api_base"),"route":data.get("route"),"cache_key":request.get("prompt_cache_key")})).ok()?;
    Some(Prompt {
        model: request["model"].as_str().unwrap_or(fallback).to_owned(),
        input,
        protected_input,
        prefix,
        overhead,
        route,
        cached_at_ms: None,
        write_tariff_known: carry::models::cache_write_tariff_known(request),
        cache_mode: ScenarioCachePolicy::for_model(request["model"].as_str().unwrap_or(fallback))
            .request_mode(request),
    })
}

/// Denominator is the alternative *scenario* cost, never the observed ledger.
pub fn savings_percent(savings: f64, carry_cost: f64) -> Option<f64> {
    let alternative = carry_cost + savings;
    (alternative.is_finite() && alternative > 0.0).then_some(100.0 * savings / alternative)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("savings_cache_write_tests.rs");
    include!("savings_initialization_tests.rs");
    fn request(t: &mut Trajectory, units: u64) {
        t.record(&json!({"event":"model_request","data":{"step":1,"request":{"input":["a ".repeat(units as usize)],"tools":[]}}}));
    }
    fn response(t: &mut Trajectory, input: u64, output: u64, proxy_output: u64) -> Estimate {
        t.record(&json!({"event":"model_response","data":{"step":1,"usage":{"input_tokens":input,"output_tokens":output},"raw":{"output":"a ".repeat(proxy_output as usize)}}})).unwrap()
    }
    #[test]
    fn session_projection_uses_the_same_tokenizer_as_the_proxy() {
        let body = json!({"model":"gpt-6-sol", "input":[{"role":"user", "content":"hello ".repeat(2000)}], "tools":[], "text":{"format":{"type":"json_object"}}});
        let mut t = Trajectory::default();
        t.record(&json!({"event":"model_request", "data":{"request":body}}));
        let rendered = json!({"overhead":{"instructions":body.get("instructions"),"tools":body.get("tools"),"text":body.get("text")}, "input":body["input"]});
        assert_eq!(
            t.pending.unwrap().input,
            crate::proxy_savings::token_units(&rendered)
        );
    }

    #[test]
    fn traditional_summary_checks_every_response_boundary() {
        for kind in [
            "message",
            "function_call",
            "custom_tool_call",
            "computer_call",
        ] {
            for crossing in [false, true] {
                let body = json!({"model": "gpt-6-luna", "input": ["evidence ".repeat(2000)]});
                let output = json!([{"type": kind, "call_id": "call_1"}]);
                let total = prompt_projection(&body).unwrap().2 + token_units(&output);
                let mut t = Trajectory::with_scenario(Scenario {
                    context_window: total + 100 - u64::from(crossing),
                    reserve: 100,
                    keep_recent: 1500,
                    summary: 100,
                });
                t.record(&json!({"event": "model_request", "data": {"request": body}}));
                let estimate = t.record(&json!({"event": "model_response", "data": {
                    "usage": {"input_tokens": 100, "output_tokens": 3}, "raw": {"output": output}
                }})).unwrap();
                assert!(estimate.priced);
                assert_eq!(estimate.pi_compactions, usize::from(crossing), "{kind}");
                assert_eq!(estimate.summary_cost > 0.0, crossing);
                if crossing {
                    assert_eq!(t.last_pi_input, 1600);
                    assert_eq!(t.last_output_proxy, 0);
                    assert!(t.pi_edited);
                }
                for event in ["turn_finished", "run_finished"] {
                    let after = t.record(&json!({"event": event, "data": {}})).unwrap();
                    assert_eq!(after.pi_compactions, estimate.pi_compactions);
                    assert_eq!(after.summary_cost, estimate.summary_cost);
                }
                if crossing {
                    let mut next = body.clone();
                    next["input"].as_array_mut().unwrap().push(output.clone());
                    t.record(&json!({"event": "model_request", "data": {"request": next}}));
                    let next_input = (t.pending.as_ref().unwrap().input as i128 - t.removed as i128
                        + t.summary as i128) as u64;
                    let cold = input_cost("gpt-6-luna", next_input, 0, Some(1024)).unwrap()
                        + scenario_output_cost("gpt-6-luna", next_input, 3).unwrap();
                    let after = t.record(&json!({"event": "model_response", "data": {
                        "usage": {"input_tokens": 100, "output_tokens": 3}, "raw": {"output": []}
                    }})).unwrap();
                    assert!(after.priced);
                    assert_eq!(after.pi_compactions, 1);
                    assert_eq!(after.summary_cost, estimate.summary_cost);
                    assert!(
                        (after.pi_cost - estimate.pi_cost - cold).abs() < 1e-12,
                        "{kind}: first request after a summary must be priced cold"
                    );
                }
            }
        }
    }

    #[test]
    fn frozen_output_cost_uses_each_branch_input_tier() {
        const FROZEN_OUTPUT: u64 = 10_000;
        const TOLERANCE: f64 = 1e-12;
        for (repetitions, pi_input) in [(2_000, 272_000_u64), (2_000, 272_001), (272_001, 272_000)]
        {
            let body = json!({
                "model": "gpt-6.1-sol",
                "input": [{"role": "user", "content": "a ".repeat(repetitions)}],
                "prompt_cache_key": "scenario"
            });
            let carry_input = prompt_projection(&body).unwrap().2;
            assert!(carry_input >= 1_024);
            assert_eq!(carry_input > 272_000, repetitions > 272_000);
            let dropped = i64::try_from(pi_input as i128 - carry_input as i128).unwrap();
            let replay = |output| {
                let mut trajectory = Trajectory::with_scenario(Scenario {
                    context_window: 1_050_000,
                    ..Scenario::default()
                });
                trajectory.record(&json!({
                    "event": "run_started",
                    "data": {
                        "model": "gpt-6.1-sol", "cache_ttl_seconds": 1800,
                        "implicit_cache_minimum_prefix_tokens": 1024
                    }
                }));
                trajectory.record(&json!({
                    "event": "context_compacted",
                    "data": {"compaction_token_delta": dropped}
                }));
                trajectory.record(&json!({
                    "event": "model_request", "run_id": "tier-test", "timestamp_ms": 100,
                    "data": {"step": 1, "route": "route", "request": body}
                }));
                let estimate = trajectory
                    .record(&json!({
                        "event": "model_response", "run_id": "tier-test", "timestamp_ms": 101,
                        "data": {
                            "step": 1, "usage": {"input_tokens": 17, "output_tokens": output},
                            "raw": {"output": [{"type": "message", "role": "assistant",
                                "content": [{"type": "output_text", "text": "done"}]}]}
                        }
                    }))
                    .unwrap();
                assert!(estimate.priced && estimate.observed_priced);
                assert_eq!(estimate.coverage.responses, 1);
                assert_eq!(estimate.pi_compactions, 0);
                assert_eq!(estimate.summary_cost, 0.0);
                estimate
            };
            // Input telemetry deliberately stays at 17: each scenario branch must
            // select its own tier from the local projection, not the native ledger.
            let zero = replay(0_u64);
            let frozen = replay(FROZEN_OUTPUT);
            let input_cost =
                |input: u64| input as f64 * if input > 272_000 { 5.0 } else { 2.5 } / 1_000_000.0;
            let output_cost = |input: u64| if input > 272_000 { 0.15 } else { 0.10 };
            let carry_output = output_cost(carry_input);
            let pi_output = output_cost(pi_input);
            // Independent rate-card values; do not use the estimator as its oracle.
            assert!((zero.carry_cost - input_cost(carry_input)).abs() < TOLERANCE);
            assert!((zero.pi_cost - input_cost(pi_input)).abs() < TOLERANCE);
            assert!((frozen.observed_cost - zero.observed_cost - 0.10).abs() < TOLERANCE);
            assert!(
                (frozen.carry_cost - zero.carry_cost - carry_output).abs() < TOLERANCE,
                "Carry input {carry_input}: frozen output must cost {carry_output} USD"
            );
            assert!(
                (frozen.pi_cost - zero.pi_cost - pi_output).abs() < TOLERANCE,
                "Pi input {pi_input}: frozen output must cost {pi_output} USD"
            );
            assert!(
                (frozen.savings - zero.savings - (pi_output - carry_output)).abs() < TOLERANCE,
                "cross-tier output contributes exactly +/-0.05 USD, not a common charge"
            );
            assert!((frozen.carry_cost - input_cost(carry_input) - carry_output).abs() < TOLERANCE);
            assert!((frozen.pi_cost - input_cost(pi_input) - pi_output).abs() < TOLERANCE);
        }
    }

    #[test]
    fn missing_projection_never_uses_provider_input_as_proxy() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        t.record(&json!({"event":"context_compacted","data":{"compaction_token_delta":40000}}));
        let e = response(&mut t, 220000, 100, 0);
        assert!(!e.priced);
        assert!(e.observed_cost > 0.0);
    }
    #[test]
    fn missing_native_removal_projection_invalidates_only_scenario() {
        for delta in [
            json!(null),
            json!(-1),
            json!(1.5),
            json!("600"),
            json!(true),
            json!(0),
        ] {
            let mut t = Trajectory::with_model("gpt-6-sol".into());
            request(&mut t, 2000);
            let before = response(&mut t, 10, 0, 0);
            let valid = delta.as_i64().is_some();
            t.record(&json!({"event":"context_compacted","data":{"compaction_token_delta":delta}}));
            request(&mut t, 1000);
            let after = response(&mut t, 10, 0, 0);
            assert_eq!(after.priced, valid, "delta: {delta}");
            assert_eq!(after.coverage.missing_projection, usize::from(!valid));
            assert!(after.observed_priced);
            assert!((after.observed_cost - 2.0 * before.observed_cost).abs() < 1e-12);
            // Later valid telemetry cannot backfill the unknown removal.
            t.record(&json!({"event":"context_compacted","data":{"compaction_token_delta":600}}));
            assert_eq!(t.snapshot().priced, valid);
            assert_eq!(t.restored, 600 + delta.as_i64().unwrap_or(0) as i128);
        }
    }
    #[test]
    fn monetary_scenario_does_not_depend_on_metered_input_units() {
        fn replay(metered: u64) -> Estimate {
            let mut t = Trajectory::with_model("gpt-6-sol".into());
            for dropped in [0, 600] {
                t.record(
                    &json!({"event":"context_compacted","data":{"compaction_token_delta":dropped}}),
                );
                request(&mut t, 2000);
                response(&mut t, metered, 100, 0);
            }
            t.snapshot()
        }
        let a = replay(2000);
        let b = replay(20000);
        assert!((a.savings - b.savings).abs() < 1e-12);
        assert_ne!(a.observed_cost, b.observed_cost);
        assert_ne!(b.carry_cost, b.observed_cost);
    }
    #[test]
    fn response_boundary_charges_summary_without_duplicate_terminal_charge() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 240000);
        let after_response = response(&mut t, 240000, 20000, 20000);
        assert_eq!(after_response.pi_compactions, 1);
        assert!(after_response.summary_cost > 0.0);
        let e = t
            .record(&json!({"event":"run_finished","data":{}}))
            .unwrap();
        assert_eq!(e.pi_compactions, 1);
        assert!(e.summary_cost > 0.0);
        assert!(e.pi_cost > e.carry_cost);
        assert_eq!(e.summary_cost, after_response.summary_cost);
        assert_eq!(e.pi_cost, after_response.pi_cost);
        assert_eq!(
            t.record(&json!({"event":"run_finished","data":{}}))
                .unwrap()
                .pi_compactions,
            1
        );
    }
    #[test]
    fn summary_is_retained_once_in_the_next_prompt() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 240000);
        let prior_total =
            t.pending.as_ref().unwrap().input + token_units(&json!("a ".repeat(20000)));
        let after_response = response(&mut t, 240000, 20000, 20000);
        assert_eq!(after_response.pi_compactions, 1);
        t.record(&json!({"event":"run_finished","data":{}}));
        request(&mut t, 261000);
        let raw_input = t.pending.as_ref().unwrap().input;
        response(&mut t, 261000, 100, 100);
        assert_eq!(
            t.last_pi_input,
            raw_input - prior_total + t.scenario.keep_recent + t.scenario.summary
        );
    }
    #[test]
    fn previous_output_is_not_added_to_the_next_prompt() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 240000);
        response(&mut t, 240000, 10000, 10000);
        request(&mut t, 250500);
        response(&mut t, 250500, 100, 100);
        assert_eq!(
            t.record(&json!({"event":"turn_finished","data":{}}))
                .unwrap()
                .pi_compactions,
            0
        );
    }
    #[test]
    fn cache_policy_charges_first_write_and_model_switch_symmetrically() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 2000);
        let a = response(&mut t, 10000, 100, 0);
        let units = t.previous.as_ref().unwrap().input;
        let write = estimated_cost_usd(
            "gpt-6-sol",
            &Usage {
                input_tokens: units,
                cache_write_input_tokens: units,
                output_tokens: 100,
                ..Usage::default()
            },
        )
        .unwrap();
        assert!((a.carry_cost - write).abs() < 1e-12);
        assert_eq!(a.carry_cost, a.pi_cost);
        t.model = "gpt-5.6-sol".into();
        request(&mut t, 2000);
        let b = response(&mut t, 10000, 100, 0);
        let switched = estimated_cost_usd(
            "gpt-5.6-sol",
            &Usage {
                input_tokens: units,
                cache_write_input_tokens: units,
                output_tokens: 100,
                ..Usage::default()
            },
        )
        .unwrap();
        assert!((b.carry_cost - write - switched).abs() < 1e-12);
        assert_eq!(b.carry_cost, b.pi_cost);
    }
    #[test]
    fn priced_usage_with_unknown_cache_policy_excludes_savings() {
        let mut t = Trajectory::default();
        t.record(&json!({"event":"run_started","data":{"model":"gpt-6-sol","implicit_cache_minimum_prefix_tokens":null}}));
        request(&mut t, 2000);
        let estimate = response(&mut t, 1000, 100, 900);
        assert!(estimate.observed_priced);
        assert_eq!(estimate.coverage.unknown_cache_requests, 1);
        assert!(!estimate.priced);
    }

    #[test]
    fn cache_policy_transition_requires_a_new_write() {
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for event in ["run_started", "session_resumed"] {
            for old_minimum in [json!(null), json!(4096)] {
                let mut t = Trajectory::default();
                t.record(&json!({"event":"run_started","data":{"model":"gpt-6-sol","implicit_cache_minimum_prefix_tokens":old_minimum}}));
                request(&mut t, 2000);
                let before = response(&mut t, 10, 0, 0);
                let carry_input = t.previous.as_ref().unwrap().input;
                let prior_pi_input = t.last_pi_input;
                t.record(
                    &json!({"event":"context_compacted","data":{"compaction_token_delta":600}}),
                );
                t.record(&json!({"event":event,"data":{"model":"gpt-6-sol","implicit_cache_minimum_prefix_tokens":1024}}));
                assert_eq!(t.last_pi_input, prior_pi_input);
                request(&mut t, 2000);
                let after = response(&mut t, 10, 0, 0);
                assert!(after.observed_priced);
                assert_eq!(after.priced, !old_minimum.is_null());
                assert_eq!(t.last_pi_input, carry_input + 600);
                assert!((after.observed_cost - 2.0 * before.observed_cost).abs() < 1e-12);
                let carry_delta = after.carry_cost - before.carry_cost;
                let pi_delta = after.pi_cost - before.pi_cost;
                let cold = input_cost("gpt-6-sol", carry_input + 600, 0, Some(1024)).unwrap();
                println!(
                    "{event} {old_minimum}->1024: carry={carry_delta:.12}, pi={pi_delta:.12}, cold_pi={cold:.12}"
                );
                actual.push((carry_delta * 1e9).round() as u64);
                actual.push((pi_delta * 1e9).round() as u64);
                expected.push(
                    (input_cost("gpt-6-sol", carry_input, 0, Some(1024)).unwrap() * 1e9).round()
                        as u64,
                );
                expected.push((cold * 1e9).round() as u64);
            }
        }
        assert_eq!(actual, expected);
    }
    #[test]
    fn cache_policy_transition_uses_the_previous_request_model() {
        let mut t = Trajectory::default();
        t.record(&json!({"event":"run_started","data":{"model":"gpt-6-sol","implicit_cache_minimum_prefix_tokens":null}}));
        request(&mut t, 2000);
        let before = response(&mut t, 10, 0, 0);
        let units = t.last_pi_input;
        // The request model can differ from the new run's fallback model.
        t.record(&json!({"event":"session_resumed","data":{"model":"gpt-5.6-sol","implicit_cache_minimum_prefix_tokens":null}}));
        t.record(&json!({"event":"model_request","data":{"step":1,"request":{"model":"gpt-6-sol","input":["a ".repeat(2000)],"tools":[]}}}));
        let after = response(&mut t, 10, 0, 0);
        let cold = input_cost("gpt-6-sol", units, 0, Some(1024)).unwrap();
        assert!((after.carry_cost - before.carry_cost - cold).abs() < 1e-12);
        assert!((after.pi_cost - before.pi_cost - cold).abs() < 1e-12);
    }
    #[test]
    fn subminimum_prompt_never_gets_cache_reads_or_writes() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 1000);
        response(&mut t, 1000, 100, 0);
        let units = t.previous.as_ref().unwrap().input;
        request(&mut t, 1000);
        let e = response(&mut t, 1000, 100, 0);
        let ordinary = estimated_cost_usd(
            "gpt-6-sol",
            &Usage {
                input_tokens: units,
                output_tokens: 100,
                ..Usage::default()
            },
        )
        .unwrap();
        assert!((e.carry_cost - ordinary * 2.0).abs() < 1e-12);
        assert_eq!(e.savings, 0.0);
    }
    #[test]
    fn recorded_cache_boundary_resets_both_branches() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 2000);
        let a = response(&mut t, 2000, 100, 0);
        t.record(&json!({"event":"cache_expired","data":{}}));
        request(&mut t, 2000);
        let b = response(&mut t, 2000, 100, 0);
        assert!((b.carry_cost - 2.0 * a.carry_cost).abs() < 1e-12);
        assert_eq!(b.carry_cost, b.pi_cost);
        t.record(&json!({"event":"model_request","data":{"step":1,"transport":{"api_base":"another-route"},"request":{"input":["a ".repeat(2000)],"tools":[]}}}));
        let c = response(&mut t, 2000, 100, 0);
        assert!((c.carry_cost - 3.0 * a.carry_cost).abs() < 1e-12);
        assert_eq!(c.carry_cost, c.pi_cost);
    }
    #[test]
    fn coverage_exposes_unanswered_and_unmetered_requests() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 2000);
        response(&mut t, 2000, 100, 0);
        request(&mut t, 2000);
        let pending = t.snapshot();
        assert_eq!(pending.coverage.unmatched_requests, 1);
        assert!(!pending.observed_priced);
        assert!(!pending.priced);
        let missing = t
            .record(&json!({"event":"model_response","data":{"step":1}}))
            .unwrap();
        assert_eq!(missing.coverage.missing_usage, 1);
        assert_eq!(missing.coverage.unmatched_requests, 0);
        assert_eq!(missing.coverage.responses, 2);
        assert_eq!(missing.observed_cost, pending.observed_cost);
        assert!(!missing.priced);
    }
    #[test]
    fn response_identity_cannot_consume_a_different_pending_request() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        t.record(&json!({"event":"model_request","run_id":"a","data":{"step":7,"request":{"input":["abc"]}}}));
        let e=t.record(&json!({"event":"model_response","run_id":"b","data":{"step":8,"usage":{"input_tokens":10,"output_tokens":1},"raw":{"output":[]}}})).unwrap();
        assert_eq!(e.coverage.unmatched_requests, 1);
        assert_eq!(e.coverage.orphan_responses, 1);
        assert!(!e.priced);
        t.record(&json!({"event":"model_response","run_id":"a","data":{"step":7,"usage":{"input_tokens":10,"output_tokens":1},"raw":{"output":[]}}}));
        assert_eq!(t.snapshot().coverage.unmatched_requests, 0);
    }
    #[test]
    fn retries_are_known_count_but_unknown_extra_charges() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 2000);
        let e=t.record(&json!({"event":"model_response","data":{"step":1,"response_retries":2,"usage":{"input_tokens":2000,"output_tokens":100},"raw":{"output":[]}}})).unwrap();
        assert_eq!(e.coverage.retries, 2);
        assert!(!e.observed_priced);
        assert!(!e.priced);
        assert!(e.observed_cost > 0.0);
    }
    #[test]
    fn absent_output_projection_cannot_silently_skip_threshold_work() {
        let mut t = Trajectory::with_model("gpt-6-sol".into());
        request(&mut t, 240000);
        let e=t.record(&json!({"event":"model_response","data":{"step":1,"usage":{"input_tokens":240000,"output_tokens":20000}}})).unwrap();
        assert!(!e.priced);
        assert!(e.observed_priced);
    }
    #[test]
    fn recorded_ttl_and_minimum_override_cache_assumptions() {
        let mut t = Trajectory::default();
        t.record(&json!({"event":"run_started","data":{"model":"gpt-6-sol","cache_ttl_seconds":1,"implicit_cache_minimum_prefix_tokens":1024,"context_window":100000}}));
        let req = |time| json!({"event":"model_request","timestamp_ms":time,"data":{"step":1,"request":{"input":["a ".repeat(2000)],"tools":[]}}});
        t.record(&req(10));
        let a=t.record(&json!({"event":"model_response","timestamp_ms":20,"data":{"step":1,"usage":{"input_tokens":2000,"output_tokens":100},"raw":{"output":[]}}})).unwrap();
        t.record(&req(1020));
        let b = response(&mut t, 2000, 100, 0);
        assert!((b.carry_cost - 2.0 * a.carry_cost).abs() < 1e-12);
        assert_eq!(b.carry_cost, b.pi_cost);
        assert_eq!(t.scenario.context_window, 100000);
    }
    #[test]
    fn missing_new_model_window_reverts_to_configured_scenario() {
        let mut t = Trajectory::with_scenario(Scenario {
            context_window: 300000,
            ..Scenario::default()
        });
        t.record(
            &json!({"event":"run_started","data":{"model":"gpt-6-sol","context_window":100000}}),
        );
        t.record(&json!({"event":"session_resumed","data":{"model":"gpt-5.6-sol"}}));
        assert_eq!(t.scenario.context_window, 300000);
        assert!(!t.snapshot().recorded_window);
    }
    #[test]
    fn invalid_window_cannot_produce_a_priced_scenario() {
        let mut t = Trajectory::with_scenario(Scenario {
            context_window: 1,
            ..Scenario::default()
        });
        t.model = "gpt-6-sol".into();
        request(&mut t, 1000);
        assert!(!response(&mut t, 1000, 100, 0).priced);
    }
    #[test]
    fn percentage_uses_alternative_cost_and_handles_zero() {
        assert_eq!(savings_percent(4.0, 1.0), Some(80.0));
        assert_eq!(savings_percent(0.0, 0.0), None);
        assert_eq!(savings_percent(-1.0, 1.0), None);
    }
}
