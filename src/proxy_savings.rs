//! Proxy-specific coverage and lifecycle around the shared savings estimator.
use crate::openai::{Usage, estimated_cost_usd, prompt_cache_capabilities};
pub(crate) use crate::savings::{Scenario, token_units};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Default, Debug, Deserialize, Serialize)]
pub struct Coverage {
    pub primary_responses: u64,
    pub reviewer_responses: u64,
    pub missing_usage: u64,
    pub failed_calls: u64,
    pub unmodeled_calls: u64,
    pub prior_untracked_calls: u64,
    pub unknown_cache_requests: u64,
}

#[derive(Clone, Default, Debug, Deserialize, Serialize)]
pub struct Estimate {
    pub carry_cost: f64,
    pub traditional_cost: f64,
    pub reviewer_cost: f64,
    pub summary_cost: f64,
    pub traditional_compactions: u64,
    pub savings: f64,
    pub savings_percent: Option<f64>,
    pub priced: bool,
    #[serde(default)]
    pub excluded_failed_calls: u64,
    pub coverage: Coverage,
}

/// Public dashboard projection: unavailable accounting is null, never a
/// plausible partial subtotal. Persisted Estimate retains its legacy shape.
#[derive(Debug, Serialize)]
pub struct DashboardEstimate {
    carry_cost: Option<f64>,
    traditional_cost: Option<f64>,
    reviewer_cost: Option<f64>,
    summary_cost: Option<f64>,
    traditional_compactions: u64,
    savings: Option<f64>,
    savings_percent: Option<f64>,
    excluded_failed_calls: u64,
    coverage: Coverage,
}

impl Estimate {
    pub fn dashboard(self, pending: bool) -> DashboardEstimate {
        let available = self.priced && !pending;
        DashboardEstimate {
            carry_cost: available.then_some(self.carry_cost),
            traditional_cost: available.then_some(self.traditional_cost),
            reviewer_cost: available.then_some(self.reviewer_cost),
            summary_cost: available.then_some(self.summary_cost),
            traditional_compactions: self.traditional_compactions,
            savings: available.then_some(self.savings),
            savings_percent: self.savings_percent.filter(|_| available),
            excluded_failed_calls: self.excluded_failed_calls,
            coverage: self.coverage,
        }
    }
}

#[derive(Default, Debug, Serialize)]
pub struct DashboardTotals {
    carry_cost: Option<f64>,
    traditional_cost: Option<f64>,
    savings: Option<f64>,
    savings_percent: Option<f64>,
    included_sessions: u64,
    pub excluded_sessions: u64,
    excluded_failed_calls: u64,
}

impl DashboardTotals {
    pub fn observe(&mut self, estimate: &DashboardEstimate) {
        let (Some(carry), Some(traditional)) = (estimate.carry_cost, estimate.traditional_cost)
        else {
            self.excluded_sessions += 1;
            return;
        };
        self.included_sessions += 1;
        self.excluded_failed_calls += estimate.excluded_failed_calls;
        let carry = self.carry_cost.unwrap_or(0.0) + carry;
        let traditional = self.traditional_cost.unwrap_or(0.0) + traditional;
        let savings = traditional - carry;
        self.carry_cost = Some(carry);
        self.traditional_cost = Some(traditional);
        self.savings = Some(savings);
        self.savings_percent = (traditional > 0.0).then_some(100.0 * savings / traditional);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Prompt {
    model: String,
    // One prior prompt, not an unbounded collection. Needed to prove append
    // compatibility, independent of whether the provider accepted a cache hit.
    prefix: String,
    overhead: String,
    route: String,
    input: u64,
    at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Trajectory {
    pub scenario: Scenario,
    result: Estimate,
    previous: Option<Prompt>,
    restored: i128,
    retired: u64,
    summary: u64,
    last_traditional_input: u64,
    carry_edited: bool,
    traditional_edited: bool,
    available: bool,
    // Old checkpoints retain their fail-closed accounting contract. Only new
    // trajectories explicitly opt into excluding proven unsuccessful attempts.
    #[serde(default)]
    successful_requests_only: bool,
}
impl Default for Trajectory {
    fn default() -> Self {
        Self {
            scenario: Scenario::default(),
            result: Estimate::default(),
            previous: None,
            restored: 0,
            retired: 0,
            summary: 0,
            last_traditional_input: 0,
            carry_edited: false,
            traditional_edited: false,
            available: true,
            successful_requests_only: true,
        }
    }
}
impl Trajectory {
    pub fn snapshot(&self) -> Estimate {
        let mut estimate = self.result.clone();
        estimate.savings = estimate.traditional_cost - estimate.carry_cost;
        estimate.priced = self.available
            && self.scenario.valid()
            && estimate.coverage.primary_responses > 0
            && estimate.coverage.unknown_cache_requests == 0;
        estimate.savings_percent = estimate
            .priced
            .then(|| crate::savings::savings_percent(estimate.savings, estimate.carry_cost))
            .flatten();
        estimate
    }

    pub fn prior_untracked(&mut self, calls: u64) {
        self.result.coverage.prior_untracked_calls += calls;
        if calls > 0 {
            self.available = false;
        }
    }

    pub fn failed_call(&mut self) {
        self.result.coverage.failed_calls += 1;
        if self.successful_requests_only {
            self.result.excluded_failed_calls += 1;
        } else {
            self.available = false;
        }
    }

    pub fn unmodeled_call(&mut self) {
        self.result.coverage.unmodeled_calls += 1;
        self.available = false;
    }

    pub fn reset_epoch(&mut self) {
        // Totals survive actual rebases/native summaries; cache and restoration
        // assumptions do not cross an ancestry discontinuity.
        self.previous = None;
        self.restored = 0;
        self.retired = 0;
        self.summary = 0;
        self.last_traditional_input = 0;
        self.carry_edited = false;
        self.traditional_edited = false;
    }

    pub fn compacted(&mut self, before: u64, after: u64) {
        // Restore the net input difference, including newly rendered memories.
        self.restored = self.restored.saturating_add(before as i128 - after as i128);
        self.carry_edited = true;
    }

    pub fn reviewer(&mut self, body: &Value, response: &Value, standard: bool) {
        if response["status"] != "completed" {
            self.failed_call();
            return;
        }
        self.reviewer_usage(
            body["model"].as_str().unwrap_or(""),
            token_units(body),
            &response["usage"],
            standard,
        );
    }

    pub fn reviewer_usage(&mut self, model: &str, input: u64, usage: &Value, standard: bool) {
        self.result.coverage.reviewer_responses += 1;
        let cost = usage["output_tokens"]
            .as_u64()
            .filter(|_| standard && usage["input_tokens"].as_u64().is_some())
            .and_then(|output| {
                estimated_cost_usd(
                    model,
                    &Usage {
                        input_tokens: input,
                        output_tokens: output,
                        ..Usage::default()
                    },
                )
            });
        if let Some(cost) = cost {
            // Reviewer has no traditional counterpart. No hypothetical cache
            // discount here: native cached tokens cannot be subtracted from a
            // local input estimate. Priced reviewer subtotal is also itemized.
            self.result.reviewer_cost += cost;
            self.result.carry_cost += cost;
        } else {
            self.result.coverage.missing_usage += 1;
            self.available = false;
        }
    }

    pub fn primary(
        &mut self,
        body: &Value,
        response: &Value,
        route: &str,
        at: u64,
        standard: bool,
    ) {
        self.result.coverage.primary_responses += 1;
        if has_native_modalities(body)
            || has_native_modalities(&response["output"])
            || ["input_tokens_details", "output_tokens_details"]
                .iter()
                .any(|field| {
                    response["usage"][*field]["audio_tokens"]
                        .as_u64()
                        .is_some_and(|tokens| tokens > 0)
                })
        {
            self.unmodeled_call();
            return;
        }
        let Some(prompt) = projection(body, route, at) else {
            self.result.coverage.missing_usage += 1;
            self.available = false;
            return;
        };
        let Some(output) = response["usage"]["output_tokens"]
            .as_u64()
            .filter(|_| standard && response["usage"]["input_tokens"].as_u64().is_some())
        else {
            self.result.coverage.missing_usage += 1;
            self.available = false;
            return;
        };
        let traditional_input = (prompt.input as i128)
            .saturating_add(self.restored)
            .saturating_sub(self.retired as i128)
            .saturating_add(self.summary as i128)
            .clamp(0, u64::MAX as i128) as u64;
        let compatible = self.previous.as_ref().is_some_and(|previous| {
            previous.model == prompt.model
                && previous.overhead == prompt.overhead
                && previous.route == prompt.route
                && at >= previous.at
                && at - previous.at < 1800
        });
        let append = self
            .previous
            .as_ref()
            .is_some_and(|previous| prompt.prefix.starts_with(&previous.prefix));
        let carry_cached = if compatible && append && !self.carry_edited {
            self.previous.as_ref().unwrap().input.min(prompt.input)
        } else {
            0
        };
        // After a Carry edit the alternative's literal prefix is unknown. The
        // scenario explicitly assumes frozen-size append reuse until its summary.
        let traditional_cached =
            if compatible && (append || self.carry_edited) && !self.traditional_edited {
                self.last_traditional_input.min(traditional_input)
            } else {
                0
            };
        let minimum =
            prompt_cache_capabilities(&prompt.model).map(|cap| cap.minimum_prefix_tokens as u64);
        if minimum.is_none() {
            self.result.coverage.unknown_cache_requests += 1;
        }
        let carry = scenario_input_cost(&prompt.model, prompt.input, carry_cached, minimum);
        let traditional = scenario_input_cost(
            &prompt.model,
            traditional_input,
            traditional_cached,
            minimum,
        );
        // Freeze the output count, not its rate: each branch selects its own tier.
        let carry_output =
            crate::savings::scenario_output_cost(&prompt.model, prompt.input, output);
        let traditional_output =
            crate::savings::scenario_output_cost(&prompt.model, traditional_input, output);
        if let (Some(carry), Some(traditional), Some(carry_output), Some(traditional_output)) =
            (carry, traditional, carry_output, traditional_output)
        {
            self.result.carry_cost += carry + carry_output;
            self.result.traditional_cost += traditional + traditional_output;
        } else {
            self.available = false;
        }
        self.last_traditional_input = traditional_input;
        self.carry_edited = false;
        self.traditional_edited = false;
        // Like the shared estimator, summarize at a completed turn boundary,
        // not between an assistant tool call and its result.
        let Some(out) = response["output"].as_array() else {
            self.result.coverage.missing_usage += 1;
            self.available = false;
            return;
        };
        let tool_turn = out.iter().any(|item| {
            item["type"]
                .as_str()
                .is_some_and(|ty| ty.ends_with("_call"))
        });
        let total = traditional_input.saturating_add(token_units(&response["output"]));
        if !tool_turn
            && self.available
            && self.scenario.valid()
            && total > self.scenario.context_window - self.scenario.reserve
        {
            let discarded = total.saturating_sub(self.scenario.keep_recent);
            if let Some(cost) = estimated_cost_usd(
                &prompt.model,
                &Usage {
                    input_tokens: discarded,
                    output_tokens: self.scenario.summary,
                    ..Usage::default()
                },
            ) {
                self.result.summary_cost += cost;
                self.result.traditional_cost += cost;
                self.result.traditional_compactions += 1;
                self.retired = self
                    .retired
                    .saturating_add(discarded.saturating_sub(self.summary));
                self.summary = self.scenario.summary;
                self.last_traditional_input =
                    self.scenario.keep_recent.saturating_add(self.summary);
                self.traditional_edited = true;
            } else {
                self.available = false;
            }
        }
        self.previous = Some(prompt);
    }
}

fn scenario_input_cost(model: &str, input: u64, cached: u64, minimum: Option<u64>) -> Option<f64> {
    crate::savings::scenario_input_cost(model, input, cached, minimum, estimated_cost_usd)
}

fn has_native_modalities(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    ["image", "audio", "video"]
                        .iter()
                        .any(|modality| kind.contains(modality))
                })
                || object.values().any(has_native_modalities)
        }
        Value::Array(items) => items.iter().any(has_native_modalities),
        _ => false, // Quoted source JSON remains text, not a native modality.
    }
}

fn projection(body: &Value, route: &str, at: u64) -> Option<Prompt> {
    let (prefix, overhead, input) = crate::savings::prompt_projection(body)?;
    Some(Prompt {
        model: body["model"].as_str()?.to_owned(),
        input,
        prefix,
        overhead,
        route: json!({"endpoint": route, "cache_key": body.get("prompt_cache_key")}).to_string(),
        at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(text: &str) -> Value {
        json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": text}], "prompt_cache_key": "scenario"})
    }

    fn response(input: u64) -> Value {
        json!({"status": "completed", "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]}],
            "usage": {"input_tokens": input, "output_tokens": 3, "input_tokens_details": {"cached_tokens": 0}}})
    }

    #[test]
    fn dashboard_projection_suppresses_all_money_without_changing_persisted_estimate() {
        let mut trajectory = Trajectory::default();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        trajectory.failed_call();
        let snapshot = trajectory.snapshot();
        let persisted = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            persisted["priced"], true,
            "internal checkpoint compatibility stays intact"
        );
        for pending in [false, true] {
            let dashboard = serde_json::to_value(snapshot.clone().dashboard(pending)).unwrap();
            assert!(dashboard.get("priced").is_none());
            for field in [
                "carry_cost",
                "traditional_cost",
                "reviewer_cost",
                "summary_cost",
                "savings",
                "savings_percent",
            ] {
                if pending {
                    assert!(
                        dashboard[field].is_null(),
                        "pending {field} must not expose partial sums"
                    );
                } else {
                    assert_eq!(dashboard[field], persisted[field]);
                }
            }
            for field in [
                "coverage",
                "excluded_failed_calls",
                "traditional_compactions",
            ] {
                assert_eq!(dashboard[field], persisted[field]);
            }
        }
        trajectory.prior_untracked(1);
        let invalid = trajectory.snapshot();
        assert!(
            invalid.carry_cost > 0.0,
            "invalid checkpoints retain internal accounting"
        );
        let dashboard = serde_json::to_value(invalid.dashboard(false)).unwrap();
        for field in [
            "carry_cost",
            "traditional_cost",
            "reviewer_cost",
            "summary_cost",
            "savings",
            "savings_percent",
        ] {
            assert!(dashboard[field].is_null());
        }
        assert_eq!(serde_json::to_value(snapshot).unwrap(), persisted);
    }

    #[test]
    fn dashboard_totals_accept_only_complete_estimates_and_use_traditional_denominator() {
        let mut totals = DashboardTotals::default();
        assert!(totals.carry_cost.is_none());
        assert!(totals.traditional_cost.is_none());
        assert!(totals.savings.is_none());
        assert!(totals.savings_percent.is_none());
        for (carry, traditional, expected_savings, expected_percent) in [
            (2.0, 10.0, 8.0, 80.0),
            (8.0, 2.0, 2.0, 100.0 / 6.0),
            (4.0, 2.0, 0.0, 0.0),
            (6.0, 2.0, -4.0, -25.0),
        ] {
            let estimate = Estimate {
                carry_cost: carry,
                traditional_cost: traditional,
                savings: traditional - carry,
                priced: true,
                excluded_failed_calls: 1,
                ..Estimate::default()
            }
            .dashboard(false);
            totals.observe(&estimate);
            assert_eq!(totals.savings, Some(expected_savings));
            assert!((totals.savings_percent.unwrap() - expected_percent).abs() < 1e-12);
        }
        for pending in [false, true] {
            totals.observe(
                &Estimate {
                    carry_cost: 999.0,
                    traditional_cost: 1000.0,
                    priced: pending,
                    excluded_failed_calls: 2,
                    ..Estimate::default()
                }
                .dashboard(pending),
            );
        }
        assert_eq!(totals.carry_cost, Some(20.0));
        assert_eq!(totals.traditional_cost, Some(16.0));
        assert_eq!(totals.savings, Some(-4.0));
        assert_eq!(totals.included_sessions, 4);
        assert_eq!(totals.excluded_sessions, 2);
        assert_eq!(totals.excluded_failed_calls, 4);
    }

    #[test]
    fn dashboard_zero_cost_is_not_missing_evidence() {
        let estimate = Estimate {
            priced: true,
            ..Estimate::default()
        }
        .dashboard(false);
        let public = serde_json::to_value(&estimate).unwrap();
        for field in [
            "carry_cost",
            "traditional_cost",
            "reviewer_cost",
            "summary_cost",
            "savings",
        ] {
            assert_eq!(public[field], 0.0);
        }
        let mut totals = DashboardTotals::default();
        totals.observe(&estimate);
        assert_eq!(totals.carry_cost, Some(0.0));
        assert_eq!(totals.traditional_cost, Some(0.0));
        assert_eq!(totals.savings, Some(0.0));
        assert_eq!(
            totals.savings_percent, None,
            "zero denominator has no percentage"
        );
        assert_eq!(totals.included_sessions, 1);
        assert_eq!(totals.excluded_sessions, 0);
    }

    #[test]
    fn successful_requests_continue_summaries_after_failures_and_restart() {
        let mut trajectory = Trajectory {
            scenario: Scenario {
                context_window: 2000,
                reserve: 100,
                keep_recent: 200,
                summary: 50,
            },
            ..Trajectory::default()
        };
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        let mut control = trajectory.clone();
        trajectory.failed_call();
        let mut resumed: Trajectory =
            serde_json::from_value(serde_json::to_value(&trajectory).unwrap()).unwrap();
        let body = request(&"evidence ".repeat(3000));
        for candidate in [&mut resumed, &mut control] {
            candidate.primary(&body, &response(100), "route", 101, true);
            candidate.reviewer(&request("priced review"), &response(100), true);
            candidate.primary(&body, &response(100), "route", 102, true);
        }
        let estimate = resumed.snapshot();
        let expected = control.snapshot();
        assert_eq!(estimate.coverage.primary_responses, 3);
        assert_eq!(estimate.coverage.failed_calls, 1);
        assert!(
            estimate.priced,
            "failed requests must not suppress priced successes"
        );
        assert_eq!(estimate.traditional_compactions, 1);
        assert!(estimate.summary_cost > 0.0);
        assert!(estimate.reviewer_cost > 0.0);
        assert_eq!(estimate.carry_cost, expected.carry_cost);
        assert_eq!(estimate.traditional_cost, expected.traditional_cost);
        assert_eq!(estimate.savings_percent, expected.savings_percent);
    }

    #[test]
    fn excluded_failures_leave_successful_prompt_and_summary_state_unchanged() {
        let mut trajectory = Trajectory::default();
        trajectory.primary(
            &request(&"prefix ".repeat(3000)),
            &response(100),
            "route",
            100,
            true,
        );
        trajectory.compacted(2000, 100);
        let before = serde_json::to_value(&trajectory).unwrap();
        trajectory.failed_call();
        let after = serde_json::to_value(&trajectory).unwrap();
        for field in [
            "previous",
            "restored",
            "retired",
            "summary",
            "last_traditional_input",
            "carry_edited",
            "traditional_edited",
            "available",
        ] {
            assert_eq!(after[field], before[field], "failure changed {field}");
        }
        let estimate = trajectory.snapshot();
        assert!(estimate.priced);
        assert_eq!(
            serde_json::to_value(estimate).unwrap()["excluded_failed_calls"],
            1
        );
    }

    #[test]
    fn failed_reviewer_terminals_with_usage_are_excluded_but_completed_invalid_advice_is_charged() {
        let mut trajectory = Trajectory::default();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        let before = trajectory.snapshot();
        for status in ["failed", "incomplete"] {
            let mut terminal = response(100);
            terminal["status"] = json!(status);
            trajectory.reviewer(&request("review"), &terminal, true);
        }
        let estimate = trajectory.snapshot();
        assert!(estimate.priced);
        assert_eq!(estimate.coverage.failed_calls, 2);
        assert_eq!(estimate.coverage.reviewer_responses, 0);
        assert_eq!(estimate.carry_cost, before.carry_cost);
        // Advice validity/refusal is not the completion classification.
        let mut refused = response(100);
        refused["output"] =
            json!([{"type":"message","content":[{"type":"refusal","refusal":"no"}]}]);
        trajectory.reviewer(&request("review"), &refused, true);
        assert_eq!(trajectory.snapshot().coverage.reviewer_responses, 1);
        assert!(trajectory.snapshot().reviewer_cost > 0.0);
        trajectory.reviewer(
            &request("review"),
            &json!({"status":"completed","output":[]}),
            true,
        );
        assert_eq!(trajectory.snapshot().coverage.missing_usage, 1);
        assert!(!trajectory.snapshot().priced);
    }

    #[test]
    fn legacy_checkpoints_keep_fail_closed_policy_across_epoch_and_restart() {
        let mut initial = Trajectory::default();
        initial.primary(&request("task"), &response(100), "route", 100, true);
        let mut checkpoint = serde_json::to_value(initial).unwrap();
        checkpoint
            .as_object_mut()
            .unwrap()
            .remove("successful_requests_only");
        checkpoint["result"]
            .as_object_mut()
            .unwrap()
            .remove("excluded_failed_calls");
        let mut legacy: Trajectory = serde_json::from_value(checkpoint).unwrap();
        assert!(
            legacy.snapshot().priced,
            "old complete trajectories remain readable"
        );
        legacy.failed_call();
        legacy.reset_epoch();
        let mut resumed: Trajectory =
            serde_json::from_value(serde_json::to_value(legacy).unwrap()).unwrap();
        resumed.primary(&request("new epoch"), &response(100), "route", 101, true);
        assert!(!resumed.snapshot().priced);
        assert_eq!(
            serde_json::to_value(resumed.snapshot()).unwrap()["excluded_failed_calls"],
            0
        );
    }

    #[test]
    fn exclusions_never_resurrect_unknown_successful_spend() {
        for variant in 0..4 {
            let mut trajectory = Trajectory::default();
            match variant {
                0 => trajectory.prior_untracked(1),
                1 => trajectory.unmodeled_call(),
                2 => trajectory.primary(
                    &request("task"),
                    &json!({"status":"completed","output":[]}),
                    "route",
                    100,
                    true,
                ),
                _ => {
                    let mut unknown = request("task");
                    unknown["model"] = json!("gpt-6-astra");
                    trajectory.primary(&unknown, &response(100), "route", 100, true);
                }
            }
            trajectory.failed_call();
            trajectory.reset_epoch();
            trajectory.primary(
                &request("known success"),
                &response(100),
                "route",
                101,
                true,
            );
            assert!(!trajectory.snapshot().priced);
        }
    }

    #[test]
    fn scenario_input_is_independent_of_native_input_metering() {
        let replay = |metered| {
            let mut trajectory = Trajectory::default();
            trajectory.primary(
                &request(&"evidence ".repeat(2000)),
                &response(metered),
                "route",
                100,
                true,
            );
            trajectory.compacted(1000, 0);
            trajectory.primary(
                &request("retained fact"),
                &response(metered),
                "route",
                101,
                true,
            );
            trajectory.snapshot()
        };
        let a = replay(100);
        let b = replay(100_000);
        assert!(a.priced && b.priced);
        assert_eq!(a.carry_cost, b.carry_cost);
        assert_eq!(a.traditional_cost, b.traditional_cost);
        assert_eq!(a.savings, a.traditional_cost - a.carry_cost);
    }

    #[test]
    fn added_memory_can_make_the_traditional_input_smaller_than_carry() {
        let body = request("a retained derived memory with additional quoting overhead");
        let mut trajectory = Trajectory::default();
        trajectory.compacted(20, 30);
        trajectory.primary(&body, &response(100), "route", 100, true);
        let expected = projection(&body, "route", 100).unwrap().input - 10;
        assert_eq!(
            trajectory.last_traditional_input, expected,
            "net context growth must not inflate the traditional baseline"
        );
    }

    #[test]
    fn reviewer_overhead_can_produce_a_negative_saving() {
        let mut trajectory = Trajectory::default();
        let body = request("tiny task");
        trajectory.primary(&body, &response(100), "route", 100, true);
        trajectory.reviewer(&body, &response(100), true);
        let estimate = trajectory.snapshot();
        assert!(estimate.priced);
        assert!(estimate.savings < 0.0);
        assert_eq!(estimate.savings, -estimate.reviewer_cost);
        assert!(estimate.savings_percent.unwrap() < 0.0);
    }

    #[test]
    fn conventional_summary_occurs_only_after_a_non_tool_boundary_and_survives_restart() {
        let mut trajectory = Trajectory {
            scenario: Scenario {
                context_window: 2000,
                reserve: 100,
                keep_recent: 200,
                summary: 50,
            },
            ..Trajectory::default()
        };
        let body = request(&"evidence ".repeat(3000));
        let mut tool_response = response(3000);
        tool_response["output"] =
            json!([{"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"}]);
        trajectory.primary(&body, &tool_response, "route", 100, true);
        assert_eq!(trajectory.snapshot().traditional_compactions, 0);
        trajectory.primary(&body, &response(3000), "route", 101, true);
        let before = trajectory.snapshot();
        assert_eq!(before.traditional_compactions, 1);
        assert!(before.summary_cost > 0.0);
        let mut resumed: Trajectory =
            serde_json::from_value(serde_json::to_value(&trajectory).unwrap()).unwrap();
        resumed.primary(&body, &response(3000), "route", 102, true);
        assert_eq!(
            resumed.snapshot().traditional_compactions,
            1,
            "summary is retained once, not charged repeatedly"
        );
        assert_eq!(resumed.snapshot().summary_cost, before.summary_cost);
    }

    #[test]
    fn cache_reuse_requires_compatible_route_key_and_time() {
        let body = request(&"evidence ".repeat(3000));
        let mut initial = Trajectory::default();
        initial.primary(&body, &response(100), "route", 100, true);
        let mut hit = initial.clone();
        hit.primary(&body, &response(100), "route", 101, true);
        for (route, time, key) in [
            ("different", 101, "scenario"),
            ("route", 1900, "scenario"),
            ("route", 101, "different"),
        ] {
            let mut miss = initial.clone();
            let mut changed = body.clone();
            changed["prompt_cache_key"] = json!(key);
            miss.primary(&changed, &response(100), route, time, true);
            assert!(miss.snapshot().carry_cost > hit.snapshot().carry_cost);
            assert!(miss.snapshot().traditional_cost > hit.snapshot().traditional_cost);
        }
    }

    #[test]
    fn scenario_uses_the_proxy_rate_card_including_gpt61_sol() {
        let mut body = request("task");
        body["model"] = json!("gpt-6.1-sol");
        let mut trajectory = Trajectory::default();
        trajectory.primary(&body, &response(100), "route", 100, true);
        assert!(trajectory.snapshot().priced);
        assert!(trajectory.snapshot().carry_cost > 0.0);
        assert_eq!(trajectory.snapshot().coverage.unknown_cache_requests, 0);
    }

    #[test]
    fn frozen_output_cost_uses_each_branch_input_tier() {
        const FROZEN_OUTPUT: u64 = 10_000;
        const TOLERANCE: f64 = 1e-12;
        // Exact threshold, one above it, and reversed short/long branches.
        for (repetitions, traditional_input) in
            [(2_000, 272_000_u64), (2_000, 272_001), (272_001, 272_000)]
        {
            let mut body = request(&"a ".repeat(repetitions));
            body["model"] = json!("gpt-6.1-sol");
            let carry_input = projection(&body, "route", 100).unwrap().input;
            assert!(carry_input >= 1_024);
            assert_eq!(carry_input > 272_000, repetitions > 272_000);
            let replay = |output| {
                let mut trajectory = Trajectory {
                    scenario: Scenario {
                        context_window: 1_050_000,
                        ..Scenario::default()
                    },
                    ..Trajectory::default()
                };
                // Establish the alternative size through the real compaction path.
                if traditional_input >= carry_input {
                    trajectory.compacted(traditional_input - carry_input, 0);
                } else {
                    trajectory.compacted(0, carry_input - traditional_input);
                }
                let mut completion = response(17);
                completion["usage"]["output_tokens"] = json!(output);
                trajectory.primary(&body, &completion, "route", 100, true);
                let estimate = trajectory.snapshot();
                assert!(estimate.priced);
                assert_eq!(estimate.coverage.primary_responses, 1);
                assert_eq!(estimate.traditional_compactions, 0);
                assert_eq!(estimate.summary_cost, 0.0);
                estimate
            };
            // Zero-output control isolates output charges from input/write costs.
            // The actual comparison freezes the same 10,000 outputs in BOTH branches.
            let zero = replay(0_u64);
            let frozen = replay(FROZEN_OUTPUT);
            let input_cost =
                |input: u64| input as f64 * if input > 272_000 { 5.0 } else { 2.5 } / 1_000_000.0;
            let output_cost = |input: u64| if input > 272_000 { 0.15 } else { 0.10 };
            let carry_output = output_cost(carry_input);
            let traditional_output = output_cost(traditional_input);
            // Independent published-rate expectations: cold eligible prompts use
            // cache writes ($2.50/$5.00 per million), output is $10/$15 per million.
            assert!((zero.carry_cost - input_cost(carry_input)).abs() < TOLERANCE);
            assert!((zero.traditional_cost - input_cost(traditional_input)).abs() < TOLERANCE);
            assert!(
                (frozen.carry_cost - zero.carry_cost - carry_output).abs() < TOLERANCE,
                "Carry input {carry_input}: frozen output must cost {carry_output} USD"
            );
            assert!(
                (frozen.traditional_cost - zero.traditional_cost - traditional_output).abs()
                    < TOLERANCE,
                "traditional input {traditional_input}: frozen output must cost {traditional_output} USD"
            );
            assert!(
                (frozen.savings - zero.savings - (traditional_output - carry_output)).abs()
                    < TOLERANCE,
                "cross-tier output contributes exactly +/-0.05 USD, not a common charge"
            );
            assert!((frozen.carry_cost - input_cost(carry_input) - carry_output).abs() < TOLERANCE);
            assert!(
                (frozen.traditional_cost - input_cost(traditional_input) - traditional_output)
                    .abs()
                    < TOLERANCE
            );
        }
    }

    #[test]
    fn priced_model_with_unknown_cache_support_excludes_savings() {
        let mut body = request("task");
        body["model"] = json!("gpt-6-astra");
        let mut trajectory = Trajectory::default();
        trajectory.primary(&body, &response(100), "route", 100, true);
        let estimate = trajectory.snapshot();
        assert!(estimate.carry_cost > 0.0);
        assert_eq!(estimate.coverage.unknown_cache_requests, 1);
        assert!(!estimate.priced);
        assert_eq!(estimate.savings_percent, None);
    }

    #[test]
    fn restored_unknown_cache_coverage_excludes_scenario_savings() {
        let mut trajectory = Trajectory::default();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        let mut checkpoint = serde_json::to_value(&trajectory).unwrap();
        checkpoint["result"]["coverage"]["unknown_cache_requests"] = json!(1);
        let restored: Trajectory = serde_json::from_value(checkpoint).unwrap();
        assert!(!restored.snapshot().priced);
        assert_eq!(restored.snapshot().savings_percent, None);
    }

    #[test]
    fn native_multimodal_inputs_are_not_priced_as_serialized_text() {
        for content in [
            json!([{"type": "input_image", "image_url": "opaque"}]),
            json!([{"type": "input_audio", "data": "opaque"}]),
        ] {
            let mut body = request("task");
            body["input"][0]["content"] = content;
            let mut trajectory = Trajectory::default();
            trajectory.primary(&body, &response(100), "route", 100, true);
            assert!(
                !trajectory.snapshot().priced,
                "text tokenization cannot price native modalities"
            );
            assert_eq!(trajectory.snapshot().coverage.unmodeled_calls, 1);
        }
    }

    #[test]
    fn missing_usage_failure_unknown_pricing_and_old_history_make_savings_unavailable() {
        let body = request("task");
        let mut checkpoint = serde_json::to_value(Trajectory::default()).unwrap();
        checkpoint
            .as_object_mut()
            .unwrap()
            .remove("successful_requests_only");
        // Preserve the historical fail-closed contract for legacy checkpoints.
        let mut initial: Trajectory = serde_json::from_value(checkpoint).unwrap();
        initial.primary(&body, &response(100), "route", 100, true);
        for variant in 0..5 {
            let mut trajectory = initial.clone();
            match variant {
                0 => trajectory.primary(
                    &body,
                    &json!({"status": "completed", "output": []}),
                    "route",
                    101,
                    true,
                ),
                1 => trajectory.failed_call(),
                2 => trajectory.prior_untracked(1),
                3 => trajectory.unmodeled_call(),
                _ => {
                    let mut unknown = body.clone();
                    unknown["model"] = json!("unknown-model");
                    trajectory.primary(&unknown, &response(100), "route", 101, true);
                }
            }
            assert!(!trajectory.snapshot().priced);
            assert!(trajectory.snapshot().savings_percent.is_none());
        }
    }
}
