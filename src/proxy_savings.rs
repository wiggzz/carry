//! Proxy-specific coverage and lifecycle around the shared savings estimator.
use crate::openai::{Usage, estimated_cost_usd};
pub(crate) use crate::savings::{Scenario, token_units};
use carry::models::ResolvedModel;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const RECEIPT_BASIS: &str = "receipt_anchored_v2";
pub const LEGACY_BASIS: &str = "legacy_json_v1";
const RECEIPT_COUNTERFACTUAL_BASIS: &str = "shared_receipt_input_plus_estimated_restoration_summary_visible_retained_output_conditional_append_cache";

pub fn accounting_policy() -> Value {
    json!({"default_basis": RECEIPT_BASIS, "retained_legacy_basis": LEGACY_BASIS,
        "observed_cost_basis": "provider_native_partitions_with_dispatch_frozen_tariffs",
        "counterfactual_basis": RECEIPT_COUNTERFACTUAL_BASIS,
        "counterfactual_cache_policy": format!("conditional_model_cache_v{}", carry::models::ScenarioCachePolicy::VERSION),
        "retained_counterfactual_cache_policy": "legacy_route_cache_v0",
        "cache_policy_upgrade": "prospective_cold_boundary_with_immutable_legacy_segment_no_repricing",
        "opaque_output_retention": "unknown_not_ciphertext_length",
        "claim": "conditional_counterfactual_not_measured_savings"})
}

pub fn accounting_basis_label() -> String {
    format!(
        "versioned_per_session;default={RECEIPT_BASIS};retained={LEGACY_BASIS};not_measured_savings"
    )
}

#[path = "proxy_receipts.rs"]
mod receipts;
#[path = "proxy_traditional.rs"]
mod traditional;
use traditional::{HistoryContract, TraditionalHistoryState};

#[derive(Clone, Default, Debug, Deserialize, Serialize)]
pub struct Coverage {
    pub primary_responses: u64,
    pub reviewer_responses: u64,
    pub missing_usage: u64,
    pub failed_calls: u64,
    pub unmodeled_calls: u64,
    pub prior_untracked_calls: u64,
    pub unknown_cache_requests: u64,
    #[serde(default)]
    pub unknown_rate_calls: u64,
}

/// Immutable historical comparison segment. No replay or historical repricing.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CachePolicySegment {
    pub matched_primary_responses: u64,
    pub carry_cost: f64,
    pub traditional_cost: f64,
    pub reviewer_cost: f64,
    pub summary_cost: f64,
    pub traditional_compactions: u64,
    pub coverage: Coverage,
}

impl From<&Estimate> for CachePolicySegment {
    fn from(estimate: &Estimate) -> Self {
        Self {
            matched_primary_responses: estimate.matched_primary_responses,
            carry_cost: estimate.carry_cost,
            traditional_cost: estimate.traditional_cost,
            reviewer_cost: estimate.reviewer_cost,
            summary_cost: estimate.summary_cost,
            traditional_compactions: estimate.traditional_compactions,
            coverage: estimate.coverage.clone(),
        }
    }
}

/// Cumulative comparison totals immediately before a prospective policy change.
/// These overlapping snapshots are provenance, NEVER additive cost segments.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CachePolicyBoundary {
    pub previous_version: Option<u32>,
    pub next_version: Option<u32>,
    pub cumulative: CachePolicySegment,
}

#[derive(Clone, Default, Debug, Deserialize, Serialize)]
pub struct Estimate {
    #[serde(default)]
    pub initialization_policy: String,
    #[serde(default)]
    pub initial_compactions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_initialization_segment: Option<CachePolicySegment>,
    #[serde(default)]
    pub counterfactual_cache_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_cache_policy_segment: Option<CachePolicySegment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_policy_boundaries: Vec<CachePolicyBoundary>,
    #[serde(default)]
    pub accounting_basis: String,
    #[serde(default)]
    pub matched_primary_responses: u64,
    #[serde(default)]
    pub observed_primary_cost: f64,
    #[serde(default)]
    pub observed_reviewer_cost: f64,
    #[serde(default)]
    pub unavailable_primary_cost_calls: u64,
    #[serde(default)]
    pub unavailable_reviewer_cost_calls: u64,
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
    #[serde(default)]
    pub excluded_unpriced_calls: u64,
    #[serde(default)]
    pub excluded_unpriced_reviewer_calls: u64,
    pub coverage: Coverage,
}

/// Public dashboard projection: a priced matched subset, or null when no
/// comparable primary observations exist. Omitted reviewer expense is carry-only.
#[derive(Debug, Serialize)]
pub struct DashboardEstimate {
    initialization_policy: String,
    initial_compactions: u64,
    legacy_initialization_segment: Option<CachePolicySegment>,
    counterfactual_cache_policy: String,
    legacy_cache_policy_segment: Option<CachePolicySegment>,
    cache_policy_boundaries: Vec<CachePolicyBoundary>,
    in_flight_costs_excluded: bool,
    accounting_basis: String,
    matched_primary_responses: u64,
    observed_primary_cost: Option<f64>,
    observed_reviewer_cost: Option<f64>,
    unavailable_primary_cost_calls: u64,
    unavailable_reviewer_cost_calls: u64,
    counterfactual_basis: &'static str,
    carry_cost: Option<f64>,
    traditional_cost: Option<f64>,
    reviewer_cost: Option<f64>,
    summary_cost: Option<f64>,
    traditional_compactions: u64,
    savings: Option<f64>,
    savings_percent: Option<f64>,
    excluded_failed_calls: u64,
    excluded_unpriced_calls: u64,
    excluded_unpriced_reviewer_calls: u64,
    coverage: Coverage,
}

impl Estimate {
    pub fn dashboard(self, pending: bool) -> DashboardEstimate {
        let available = self.priced;
        DashboardEstimate {
            initialization_policy: self.initialization_policy,
            initial_compactions: self.initial_compactions,
            legacy_initialization_segment: self.legacy_initialization_segment,
            counterfactual_cache_policy: self.counterfactual_cache_policy,
            legacy_cache_policy_segment: self.legacy_cache_policy_segment,
            cache_policy_boundaries: self.cache_policy_boundaries,
            in_flight_costs_excluded: pending,
            observed_primary_cost: (self.accounting_basis == RECEIPT_BASIS
                && self.coverage.primary_responses > self.unavailable_primary_cost_calls)
                .then_some(self.observed_primary_cost),
            observed_reviewer_cost: (self.accounting_basis == RECEIPT_BASIS
                && self.coverage.reviewer_responses > self.unavailable_reviewer_cost_calls)
                .then_some(self.observed_reviewer_cost),
            counterfactual_basis: if self.accounting_basis == RECEIPT_BASIS {
                RECEIPT_COUNTERFACTUAL_BASIS
            } else {
                "legacy_local_json_projection"
            },
            accounting_basis: self.accounting_basis,
            matched_primary_responses: self.matched_primary_responses,
            unavailable_primary_cost_calls: self.unavailable_primary_cost_calls,
            unavailable_reviewer_cost_calls: self.unavailable_reviewer_cost_calls,

            carry_cost: available.then_some(self.carry_cost),
            traditional_cost: available.then_some(self.traditional_cost),
            reviewer_cost: available.then_some(self.reviewer_cost),
            summary_cost: available.then_some(self.summary_cost),
            traditional_compactions: self.traditional_compactions,
            savings: available.then_some(self.savings),
            savings_percent: self.savings_percent.filter(|_| available),
            excluded_failed_calls: self.excluded_failed_calls,
            excluded_unpriced_calls: self.excluded_unpriced_calls,
            excluded_unpriced_reviewer_calls: self.excluded_unpriced_reviewer_calls,
            coverage: self.coverage,
        }
    }
}

#[derive(Default, Debug, Serialize)]
pub struct DashboardTotals {
    legacy_cache_policy_sessions: u64,
    mixed_cache_policy_sessions: u64,
    in_flight_sessions: u64,
    legacy_sessions: u64,
    observed_primary_cost: Option<f64>,
    observed_reviewer_cost: Option<f64>,
    unavailable_primary_cost_calls: u64,
    unavailable_reviewer_cost_calls: u64,
    matched_primary_responses: u64,
    carry_cost: Option<f64>,
    traditional_cost: Option<f64>,
    savings: Option<f64>,
    savings_percent: Option<f64>,
    included_sessions: u64,
    pub excluded_sessions: u64,
    excluded_failed_calls: u64,
    excluded_unpriced_calls: u64,
    excluded_unpriced_reviewer_calls: u64,
}

impl DashboardTotals {
    pub fn observe(&mut self, estimate: &DashboardEstimate) {
        self.in_flight_sessions += u64::from(estimate.in_flight_costs_excluded);
        // Diagnostics cover all sessions, even pending/ineligible comparisons.
        self.excluded_unpriced_calls += estimate.excluded_unpriced_calls;
        self.excluded_unpriced_reviewer_calls += estimate.excluded_unpriced_reviewer_calls;
        if estimate.accounting_basis == LEGACY_BASIS {
            self.legacy_sessions += 1;
            self.excluded_sessions += 1;
            return;
        }
        self.unavailable_primary_cost_calls += estimate.unavailable_primary_cost_calls;
        self.unavailable_reviewer_cost_calls += estimate.unavailable_reviewer_cost_calls;
        if let Some(cost) = estimate.observed_primary_cost {
            self.observed_primary_cost = Some(self.observed_primary_cost.unwrap_or(0.0) + cost);
        }
        if let Some(cost) = estimate.observed_reviewer_cost {
            self.observed_reviewer_cost = Some(self.observed_reviewer_cost.unwrap_or(0.0) + cost);
        }
        let (Some(carry), Some(traditional)) = (estimate.carry_cost, estimate.traditional_cost)
        else {
            self.excluded_sessions += 1;
            return;
        };
        self.included_sessions += 1;
        match estimate.counterfactual_cache_policy.as_str() {
            "legacy_route_cache_v0" => self.legacy_cache_policy_sessions += 1,
            "mixed_legacy_route_v0_and_model_v1" | "mixed_versioned_cache_policies" => {
                self.mixed_cache_policy_sessions += 1
            }
            _ => (),
        }
        self.matched_primary_responses += estimate.matched_primary_responses;
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

/// Persisted accounting has one supported representation. Older experimental
/// shapes are rejected by serde before Session can publish or extend authority.
#[derive(Clone, Debug, Deserialize, Serialize)]
enum TrajectoryFormat {
    #[serde(rename = "carry-accounting-v3")]
    Current,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Trajectory {
    format: TrajectoryFormat,
    receipts: receipts::Receipts,
    pub scenario: Scenario,
    result: Estimate,
    restored: i128,
    #[serde(flatten)]
    history: TraditionalHistoryState,
    carry_edited: bool,
    traditional_edited: bool,
    available: bool,
    priced_primary_responses: u64,
}
/// A terminal observation with money already settled under dispatch-frozen
/// tariffs. Comparison may forecast another history, never reprice this one.
#[derive(Clone, Copy)]
pub struct PrimaryObservation<'a> {
    pub body: &'a Value,
    pub response: &'a Value,
    pub route: &'a str,
    pub at: u64,
    pub standard: bool,
    pub resolved: Option<&'a ResolvedModel>,
    pub cost: Option<f64>,
}

impl Default for Trajectory {
    fn default() -> Self {
        Self {
            format: TrajectoryFormat::Current,
            receipts: receipts::Receipts::default(),
            scenario: Scenario::default(),
            result: Estimate {
                accounting_basis: RECEIPT_BASIS.into(),
                ..Estimate::default()
            },
            restored: 0,
            history: TraditionalHistoryState::identical(),
            carry_edited: false,
            traditional_edited: false,
            available: true,
            priced_primary_responses: 0,
        }
    }
}
impl Trajectory {
    pub fn snapshot(&self) -> Estimate {
        let mut estimate = self.result.clone();
        estimate.initialization_policy =
            if self.receipts.initialization_policy_version == Some(1) {
                if estimate.legacy_initialization_segment.is_some() {
                    "mixed_legacy_and_free_pre_window_v1"
                } else {
                    "unrecognized_history_free_pre_window_v1"
                }
            } else {
                "legacy_post_primary_compaction"
            }
            .into();
        let version = self.receipts.scenario_cache_policy_version;
        // Preserve the original missing -> v1 label for old checkpoint readers,
        // but never label an arbitrary present version as v1. New boundaries
        // retain each transition without replacing the first legacy segment.
        let original_upgrade = version == Some(1)
            && estimate.legacy_cache_policy_segment.is_some()
            && (estimate.cache_policy_boundaries.is_empty()
                || (estimate.cache_policy_boundaries.len() == 1
                    && estimate.cache_policy_boundaries[0]
                        .previous_version
                        .is_none()
                    && estimate.cache_policy_boundaries[0].next_version == Some(1)));
        estimate.counterfactual_cache_policy = if original_upgrade {
            "mixed_legacy_route_v0_and_model_v1".into()
        } else if !estimate.cache_policy_boundaries.is_empty()
            || estimate.legacy_cache_policy_segment.is_some()
        {
            "mixed_versioned_cache_policies".into()
        } else {
            version.map_or_else(
                || "legacy_route_cache_v0".into(),
                |v| format!("conditional_model_cache_v{v}"),
            )
        };
        estimate.accounting_basis = RECEIPT_BASIS.into();
        estimate.matched_primary_responses = self.priced_primary_responses;
        estimate.savings = estimate.traditional_cost - estimate.carry_cost;
        estimate.priced =
            self.available && self.scenario.valid() && self.priced_primary_responses > 0;
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
        // A failed primary cannot promise reuse, but must not discard the
        // successful scenario's restoration or hypothetical summary.
        self.receipts.invalidate();
        self.record_failed_call();
    }

    pub fn failed_reviewer_call(&mut self) {
        // Carry-only work did not touch either primary history or its cache.
        self.record_failed_call();
    }

    fn record_failed_call(&mut self) {
        self.result.coverage.failed_calls += 1;
        self.result.excluded_failed_calls += 1;
    }

    pub fn unmodeled_call(&mut self) {
        self.result.unavailable_primary_cost_calls += 1;
        self.result.coverage.primary_responses += 1;
        self.result.coverage.unmodeled_calls += 1;
        self.result.excluded_unpriced_calls += 1;
        // Opaque/native boundaries cannot supply a trustworthy text projection.
        self.reset_epoch();
    }

    pub fn reset_epoch(&mut self) {
        self.receipts.invalidate();
        // Totals survive actual rebases/native summaries; cache and restoration
        // assumptions do not cross an ancestry discontinuity.
        self.restored = 0;
        self.history = TraditionalHistoryState::default();
        self.carry_edited = false;
        self.traditional_edited = false;
    }

    /// Receipt compatibility invalidates historical anchors and summary state,
    /// not the already measured current request's signed disposable projection.
    pub(super) fn reset_receipt_epoch(&mut self) {
        let restored = self.restored;
        let edited = self.carry_edited;
        self.reset_epoch();
        self.restored = restored;
        self.carry_edited = edited;
    }

    /// Complete current canonical-minus-sent projection, not an additive edit.
    /// Historical charges and the alternative's own summary remain untouched.
    pub fn projected(&mut self, before: u64, after: u64, edited: bool) {
        let restored = before as i128 - after as i128;
        if edited || self.restored != restored {
            self.history.diverge();
            // Carry changed its wire, not the traditional history. The
            // explicit edit bit preserves equal-token prefix invalidation.
            self.receipts.invalidate_actual();
            self.carry_edited = true;
        }
        // Signed: a replacement memory can be larger than its original source.
        self.restored = restored;
    }

    #[cfg(test)]
    pub fn reviewer(&mut self, body: &Value, response: &Value, standard: bool) {
        self.reviewer_resolved(body, response, standard, None);
    }

    #[cfg(test)]
    pub fn reviewer_resolved(
        &mut self,
        body: &Value,
        response: &Value,
        standard: bool,
        resolved: Option<&ResolvedModel>,
    ) {
        if response["status"] != "completed" {
            self.failed_reviewer_call();
            return;
        }
        self.reviewer_usage_resolved(
            body["model"].as_str().unwrap_or(""),
            token_units(body),
            &response["usage"],
            standard,
            resolved,
            carry::models::cache_write_tariff_known(body),
        );
    }

    #[cfg(test)]
    pub fn reviewer_usage(&mut self, model: &str, input: u64, usage: &Value, standard: bool) {
        self.reviewer_usage_resolved(model, input, usage, standard, None, true);
    }

    #[cfg(test)]
    pub fn reviewer_usage_resolved(
        &mut self,
        model: &str,
        _input: u64,
        usage: &Value,
        standard: bool,
        resolved: Option<&ResolvedModel>,
        write_tariff_known: bool,
    ) {
        let settled = receipts::receipt(usage)
            .filter(|_| standard)
            .and_then(|u| observed_cost(model, &u, resolved, write_tariff_known));
        self.reviewer_usage_settled(usage, standard, settled);
    }

    /// Money is already frozen by the paid ledger, including delayed reviews.
    pub fn reviewer_usage_settled(&mut self, usage: &Value, standard: bool, settled: Option<f64>) {
        self.observed_reviewer(usage, standard, settled);
    }

    #[cfg(test)]
    pub fn primary(
        &mut self,
        body: &Value,
        response: &Value,
        route: &str,
        at: u64,
        standard: bool,
    ) {
        self.primary_resolved(body, response, route, at, standard, None);
    }

    #[cfg(test)]
    pub fn primary_resolved(
        &mut self,
        body: &Value,
        response: &Value,
        route: &str,
        at: u64,
        standard: bool,
        resolved: Option<&ResolvedModel>,
    ) {
        let settled = receipts::receipt(&response["usage"])
            .filter(|_| standard)
            .and_then(|u| {
                observed_cost(
                    body["model"].as_str().unwrap_or(""),
                    &u,
                    resolved,
                    carry::models::cache_write_tariff_known(body),
                )
            });
        self.primary_settled(PrimaryObservation {
            body,
            response,
            route,
            at,
            standard,
            resolved,
            cost: settled,
        });
    }

    pub fn primary_settled(&mut self, primary: PrimaryObservation<'_>) {
        self.observed_primary(primary);
    }
}

// Native receipt controls also exercise the shared hypothetical input tariff.
#[cfg(test)]
fn scenario_input_cost(model: &str, input: u64, cached: u64, minimum: Option<u64>) -> Option<f64> {
    scenario_input_cost_resolved(model, input, cached, minimum, None)
}

#[cfg(test)]
fn scenario_input_cost_resolved(
    model: &str,
    input: u64,
    cached: u64,
    minimum: Option<u64>,
    resolved: Option<&ResolvedModel>,
) -> Option<f64> {
    crate::savings::scenario_input_cost(model, input, cached, minimum, |model, usage| {
        modeled_cost(model, usage, resolved)
    })
}

fn request_cache_mode(
    body: &Value,
    model: &str,
    resolved: Option<&ResolvedModel>,
) -> carry::models::ScenarioCacheMode {
    match resolved {
        Some(r) => r
            .scenario_cache_policy
            .as_ref()
            .map_or(carry::models::ScenarioCacheMode::Implicit, |p| {
                p.request_mode(body)
            }),
        None => carry::models::ScenarioCachePolicy::for_model(model).request_mode(body),
    }
}

fn scenario_request_input_cost_resolved(
    body: &Value,
    model: &str,
    input: u64,
    cached: u64,
    minimum: Option<u64>,
    resolved: Option<&ResolvedModel>,
) -> Option<f64> {
    crate::savings::scenario_input_cost_with_mode(
        model,
        input,
        cached,
        minimum,
        request_cache_mode(body, model, resolved),
        |model, usage| modeled_cost(model, usage, resolved),
    )
}

fn scenario_output_cost_resolved(
    model: &str,
    input: u64,
    output: u64,
    resolved: Option<&ResolvedModel>,
) -> Option<f64> {
    match resolved {
        Some(r) if r.model == model => r
            .rates_for_input(input as f64)
            .map(|rates| output as f64 * rates.output),
        Some(_) => None,
        None => crate::savings::scenario_output_cost(model, input, output),
    }
}

#[cfg(test)]
fn observed_cost(
    model: &str,
    usage: &Usage,
    resolved: Option<&ResolvedModel>,
    write_tariff_known: bool,
) -> Option<f64> {
    if usage.cache_write_input_tokens > 0 && !write_tariff_known {
        return None;
    }
    modeled_cost(model, usage, resolved)
}

fn modeled_cost(model: &str, usage: &Usage, resolved: Option<&ResolvedModel>) -> Option<f64> {
    let Some(resolved) = resolved else {
        return estimated_cost_usd(model, usage);
    };
    if resolved.model != model {
        return None;
    }
    let rates = resolved.rates_for_input(usage.input_tokens as f64)?;
    let cached = usage.cached_input_tokens.min(usage.input_tokens);
    let written = usage
        .cache_write_input_tokens
        .min(usage.input_tokens - cached);
    Some(
        (usage.input_tokens - cached - written) as f64 * rates.input
            + cached as f64 * rates.cached
            + written as f64 * rates.write
            + usage.output_tokens as f64 * rates.output,
    )
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

#[cfg(test)]
#[path = "proxy_receipt_tests.rs"]
mod receipt_tests;

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
    fn persisted_reviewer_expense_roundtrips_without_repricing() {
        // This exact paid total exposed a one-ULP drift at a real restart.
        // Exercise the checkpoint codec, not only a Value-to-Value conversion.
        for expense in [0.00011470000000000001_f64, 0.0001147, 0.0] {
            let mut trajectory = Trajectory::default();
            trajectory.result.reviewer_cost = expense;
            let bytes = serde_json::to_vec(&trajectory).unwrap();
            let resumed: Trajectory = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                resumed.result.reviewer_cost.to_bits(),
                expense.to_bits(),
                "RECEIPT_CODEC_RED: restarting must not change already settled expense"
            );
        }
    }

    #[test]
    fn priced_subset_reviewer_omissions_preserve_known_expense_without_touching_primary_cache() {
        let mut trajectory = Trajectory::default();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        trajectory.reviewer(&request("known review"), &response(100), true);
        let before = serde_json::to_value(&trajectory).unwrap();
        trajectory.reviewer_usage("unknown-model", 100, &response(100)["usage"], true);
        trajectory.reviewer_usage("gpt-6-luna", 100, &Value::Null, true);
        let after = serde_json::to_value(&trajectory).unwrap();
        assert_eq!(after["receipts"], before["receipts"]);
        let public = serde_json::to_value(trajectory.snapshot().dashboard(false)).unwrap();
        assert_eq!(public["excluded_unpriced_calls"], 2);
        assert_eq!(public["excluded_unpriced_reviewer_calls"], 2);
        assert_eq!(public["reviewer_cost"], before["result"]["reviewer_cost"]);
        assert!(public["savings"].as_f64().unwrap() < 0.0);
        let coverage = &public["coverage"];
        assert_eq!(
            coverage["missing_usage"], 1,
            "unknown rates are not missing usage"
        );
        assert_eq!(coverage["unknown_rate_calls"], 1);
    }

    #[test]
    fn priced_subset_counters_aggregate_even_when_pending_or_ineligible() {
        let mut trajectory = Trajectory::default();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        trajectory.reviewer_usage("unknown-model", 100, &response(100)["usage"], true);
        let mut totals = DashboardTotals::default();
        totals.observe(&trajectory.snapshot().dashboard(false));
        totals.observe(&trajectory.snapshot().dashboard(true));
        trajectory.prior_untracked(1);
        totals.observe(&trajectory.snapshot().dashboard(false));
        let public = serde_json::to_value(totals).unwrap();
        assert_eq!(public["included_sessions"], 2);
        assert_eq!(public["excluded_sessions"], 1);
        assert_eq!(public["legacy_sessions"], 0);
        assert_eq!(public["excluded_unpriced_calls"], 3);
        assert_eq!(public["excluded_unpriced_reviewer_calls"], 3);
        assert!(public["carry_cost"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn dashboard_projection_keeps_completed_money_without_changing_persisted_estimate() {
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
                assert_eq!(dashboard[field], persisted[field]);
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
                    priced: false,
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
    fn excluded_failures_leave_successful_prompt_and_summary_state_unchanged() {
        let mut trajectory = Trajectory::default();
        trajectory.primary(
            &request(&"prefix ".repeat(3000)),
            &response(100),
            "route",
            100,
            true,
        );
        trajectory.projected(2000, 100, true);
        let before = serde_json::to_value(&trajectory).unwrap();
        trajectory.failed_call();
        let after = serde_json::to_value(&trajectory).unwrap();
        for field in [
            "history_continuity",
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
        assert!(
            trajectory.snapshot().priced,
            "omit completed reviewer without usage, retain priced primary"
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
            assert_eq!(
                trajectory.snapshot().priced,
                variant != 0,
                "only unknown ancestry remains unavailable"
            );
        }
    }

    #[test]
    fn added_memory_can_make_the_traditional_input_smaller_than_carry() {
        let body = request("a retained derived memory with additional quoting overhead");
        let mut trajectory = Trajectory::default();
        trajectory.projected(20, 30, true);
        trajectory.primary(&body, &response(100), "route", 100, true);
        let expected = 100 - 10;
        assert_eq!(
            trajectory.history.last_traditional_input, expected,
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
    fn scenario_uses_the_proxy_rate_card_including_gpt61_sol() {
        let mut body = request("task");
        body["model"] = json!("gpt-6.1-sol");
        let mut trajectory = Trajectory::default();
        trajectory.primary(&body, &response(100), "route", 100, true);
        assert!(trajectory.snapshot().priced);
        assert!(trajectory.snapshot().carry_cost > 0.0);
        assert_eq!(trajectory.snapshot().coverage.unknown_cache_requests, 0);
    }
}
