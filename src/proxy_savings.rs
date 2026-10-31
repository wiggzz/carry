//! Proxy-specific coverage and lifecycle around the shared savings estimator.
use crate::openai::{Usage, estimated_cost_usd, prompt_cache_capabilities};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_estimate: Option<Box<Estimate>>,
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
    legacy_estimate: Option<Box<Estimate>>,
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
            legacy_estimate: self.legacy_estimate,

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
    #[serde(default)]
    receipts: Option<receipts::Receipts>,
    pub scenario: Scenario,
    result: Estimate,
    previous: Option<Prompt>,
    restored: i128,
    #[serde(flatten)]
    history: TraditionalHistoryState,
    carry_edited: bool,
    traditional_edited: bool,
    available: bool,
    // Old checkpoints retain their fail-closed accounting contract. Only new
    // trajectories explicitly opt into excluding proven unsuccessful attempts.
    #[serde(default)]
    successful_requests_only: bool,
    // A count, not a policy/version flag. None identifies old sums whose
    // comparable primary count must be checked before extending them.
    #[serde(default)]
    priced_primary_responses: Option<u64>,
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
            receipts: Some(receipts::Receipts::default()),
            scenario: Scenario::default(),
            result: Estimate {
                accounting_basis: RECEIPT_BASIS.into(),
                ..Estimate::default()
            },
            previous: None,
            restored: 0,
            history: TraditionalHistoryState::identical(),
            carry_edited: false,
            traditional_edited: false,
            available: true,
            successful_requests_only: true,
            priced_primary_responses: Some(0),
        }
    }
}
impl Trajectory {
    pub fn snapshot(&self) -> Estimate {
        let mut estimate = self.result.clone();
        estimate.initialization_policy = if self
            .receipts
            .as_ref()
            .is_some_and(|r| r.initialization_policy_version == Some(1))
        {
            if estimate.legacy_initialization_segment.is_some() {
                "mixed_legacy_and_free_pre_window_v1"
            } else {
                "unrecognized_history_free_pre_window_v1"
            }
        } else {
            "legacy_post_primary_compaction"
        }
        .into();
        let version = self
            .receipts
            .as_ref()
            .and_then(|r| r.scenario_cache_policy_version);
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
        estimate.accounting_basis = if self.receipts.is_some() {
            RECEIPT_BASIS
        } else {
            LEGACY_BASIS
        }
        .into();
        estimate.matched_primary_responses = self.priced_primary_responses.unwrap_or(0);
        estimate.legacy_estimate = self
            .receipts
            .as_ref()
            .and_then(|r| r.legacy_estimate.clone());
        estimate.savings = estimate.traditional_cost - estimate.carry_cost;
        estimate.priced = self.available
            && self.scenario.valid()
            && self.priced_primary_responses.unwrap_or_else(|| {
                if self.legacy_subset_reconstructable() {
                    estimate.coverage.primary_responses
                } else {
                    0
                }
            }) > 0;
        // Legacy reason counters can overlap on one call, so their maximum is
        // only a lower bound. Reviewer attribution is unavailable; retain any
        // larger explicit count rather than inventing omissions by summing.
        if self.priced_primary_responses.is_none() {
            estimate.excluded_unpriced_calls = estimate
                .excluded_unpriced_calls
                .max(estimate.coverage.missing_usage)
                .max(estimate.coverage.unmodeled_calls)
                .max(estimate.coverage.unknown_cache_requests);
        }
        estimate.savings_percent = estimate
            .priced
            .then(|| crate::savings::savings_percent(estimate.savings, estimate.carry_cost))
            .flatten();
        estimate
    }

    fn legacy_subset_reconstructable(&self) -> bool {
        self.available
            && self.result.coverage.missing_usage == 0
            && self.result.coverage.unmodeled_calls == 0
            && self.result.coverage.unknown_cache_requests == 0
            && self.result.coverage.prior_untracked_calls == 0
    }

    fn start_priced_subset(&mut self) {
        if self.priced_primary_responses.is_none() {
            // Never resurrect historical sums accumulated while summaries were
            // disabled or unknown-cache costs were already included. No replay.
            self.available &= self.legacy_subset_reconstructable();
            let snapshot = self.snapshot();
            self.result.excluded_unpriced_calls = snapshot.excluded_unpriced_calls;
            self.priced_primary_responses = Some(if self.available {
                self.result.coverage.primary_responses
            } else {
                0
            });
        }
    }

    pub fn prior_untracked(&mut self, calls: u64) {
        self.result.coverage.prior_untracked_calls += calls;
        if calls > 0 {
            self.available = false;
        }
    }

    pub fn failed_call(&mut self) {
        if let Some(receipts) = &mut self.receipts {
            // A failed primary cannot promise reuse, but must not discard the
            // successful scenario's restoration or hypothetical summary.
            receipts.invalidate();
        }
        self.record_failed_call();
    }

    pub fn failed_reviewer_call(&mut self) {
        // Carry-only work did not touch either primary history or its cache.
        self.record_failed_call();
    }

    fn record_failed_call(&mut self) {
        self.result.coverage.failed_calls += 1;
        if self.successful_requests_only {
            self.result.excluded_failed_calls += 1;
        } else {
            self.available = false;
        }
    }

    pub fn unmodeled_call(&mut self) {
        if self.receipts.is_some() {
            self.result.unavailable_primary_cost_calls += 1;
        }
        self.start_priced_subset();
        self.result.coverage.primary_responses += 1;
        self.result.coverage.unmodeled_calls += 1;
        self.result.excluded_unpriced_calls += 1;
        // Opaque/native boundaries cannot supply a trustworthy text projection.
        self.reset_epoch();
    }

    pub fn reset_epoch(&mut self) {
        if let Some(receipts) = &mut self.receipts {
            receipts.invalidate();
        }
        // Totals survive actual rebases/native summaries; cache and restoration
        // assumptions do not cross an ancestry discontinuity.
        self.previous = None;
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
            if let Some(receipts) = &mut self.receipts {
                // Carry changed its wire, not the traditional history. The
                // explicit edit bit preserves equal-token prefix invalidation.
                receipts.invalidate_actual();
            }
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
        input: u64,
        usage: &Value,
        standard: bool,
        resolved: Option<&ResolvedModel>,
        write_tariff_known: bool,
    ) {
        let settled = receipts::receipt(usage)
            .filter(|_| standard)
            .and_then(|u| observed_cost(model, &u, resolved, write_tariff_known));
        self.reviewer_usage_settled(model, input, usage, standard, resolved, settled);
    }

    /// Money is already frozen by the paid ledger, including delayed reviews.
    pub fn reviewer_usage_settled(
        &mut self,
        model: &str,
        input: u64,
        usage: &Value,
        standard: bool,
        resolved: Option<&ResolvedModel>,
        settled: Option<f64>,
    ) {
        if self.receipts.is_some() {
            self.observed_reviewer(usage, standard, settled);
            return;
        }
        self.start_priced_subset();
        self.result.coverage.reviewer_responses += 1;
        let output = usage["output_tokens"]
            .as_u64()
            .filter(|_| usage["input_tokens"].as_u64().is_some());
        if output.is_none() {
            self.result.coverage.missing_usage += 1;
        } else if !standard {
            self.result.coverage.unmodeled_calls += 1;
        }
        let cost = output.filter(|_| standard).and_then(|output| {
            modeled_cost(
                model,
                &Usage {
                    input_tokens: input,
                    output_tokens: output,
                    ..Usage::default()
                },
                resolved,
            )
        });
        if let Some(cost) = cost {
            // Reviewer has no traditional counterpart. Preserve known expense;
            // omitted reviewer expense is NOT an equal-cost cancellation.
            self.result.reviewer_cost += cost;
            self.result.carry_cost += cost;
        } else {
            if output.is_some() && standard {
                self.result.coverage.unknown_rate_calls += 1;
            }
            self.result.excluded_unpriced_calls += 1;
            self.result.excluded_unpriced_reviewer_calls += 1;
        }
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
        if self.receipts.is_some() {
            self.observed_primary(primary);
            return;
        }
        let PrimaryObservation {
            body,
            response,
            route,
            at,
            standard,
            resolved,
            ..
        } = primary;
        self.start_priced_subset();
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
        self.result.coverage.primary_responses += 1;
        let (Some(prompt), Some(_)) = (projection(body, route, at), response["output"].as_array())
        else {
            // Without a structural projection, omit the entire matched pair
            // before touching money and restart cache/summary assumptions.
            self.result.coverage.missing_usage += 1;
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        };
        let output = response["usage"]["output_tokens"]
            .as_u64()
            .filter(|_| response["usage"]["input_tokens"].as_u64().is_some());
        if output.is_none() {
            self.result.coverage.missing_usage += 1;
        } else if !standard {
            self.result.coverage.unmodeled_calls += 1;
        }
        let traditional_input = (prompt.input as i128)
            .saturating_add(self.restored)
            .saturating_sub(self.history.retired as i128)
            .saturating_add(self.history.summary as i128)
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
                self.history.last_traditional_input.min(traditional_input)
            } else {
                0
            };
        let minimum = match resolved {
            Some(r) => r.cache,
            None => prompt_cache_capabilities(&prompt.model),
        }
        .map(|cap| cap.minimum_prefix_tokens as u64);
        if minimum.is_none() {
            self.result.coverage.unknown_cache_requests += 1;
        }
        if modeled_cost(&prompt.model, &Usage::default(), resolved).is_none() {
            self.result.coverage.unknown_rate_calls += 1;
        }
        // Freeze the output count, not its rate: each branch selects its own tier.
        let pair = output.filter(|_| standard).and_then(|output| {
            // Legacy scenarios required a known cache contract. Frozen routes
            // with unknown caching instead compare both branches uncached.
            if resolved.is_none() {
                minimum?;
            }
            Some((
                scenario_input_cost_resolved(
                    &prompt.model,
                    prompt.input,
                    carry_cached,
                    minimum,
                    resolved,
                )? + scenario_output_cost_resolved(&prompt.model, prompt.input, output, resolved)?,
                scenario_input_cost_resolved(
                    &prompt.model,
                    traditional_input,
                    traditional_cached,
                    minimum,
                    resolved,
                )? + scenario_output_cost_resolved(
                    &prompt.model,
                    traditional_input,
                    output,
                    resolved,
                )?,
            ))
        });
        // Advance the conventional text scenario even when usage/rates are
        // missing. Opaque modalities instead reset the structural epoch above.
        // Every completed request is a summary opportunity, including tool calls.
        // Waiting for an agent turn to finish can overshoot the scenario window.
        let total = traditional_input.saturating_add(token_units(&response["output"]));
        let summarize = self.available
            && self.scenario.valid()
            && total > self.scenario.context_window - self.scenario.reserve;
        let discarded = total.saturating_sub(self.scenario.keep_recent);
        let summary_cost = if summarize {
            modeled_cost(
                &prompt.model,
                &Usage {
                    input_tokens: discarded,
                    output_tokens: self.scenario.summary,
                    ..Usage::default()
                },
                resolved,
            )
        } else {
            Some(0.0)
        };
        // A completed primary (including its hypothetical boundary summary) is
        // one atomic matched pair. Never add only one branch or half a call.
        let priced = if let (Some((carry, traditional)), Some(summary_cost)) = (pair, summary_cost)
        {
            self.result.carry_cost += carry;
            self.result.traditional_cost += traditional + summary_cost;
            self.result.summary_cost += summary_cost;
            *self.priced_primary_responses.as_mut().unwrap() += 1;
            true
        } else {
            self.result.excluded_unpriced_calls += 1;
            false
        };
        self.history.last_traditional_input = traditional_input;
        self.carry_edited = false;
        self.traditional_edited = false;
        if summarize {
            self.result.traditional_compactions += 1;
            self.history
                .summarize(total, self.scenario.keep_recent, self.scenario.summary);
            self.traditional_edited = true;
        }
        // An unknown intervening primary must never leave a stale cache hit.
        self.previous = priced.then_some(prompt);
    }
}

#[cfg(test)]
fn scenario_input_cost(model: &str, input: u64, cached: u64, minimum: Option<u64>) -> Option<f64> {
    scenario_input_cost_resolved(model, input, cached, minimum, None)
}

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
#[path = "proxy_receipt_tests.rs"]
mod receipt_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // Preserve the executable v1 arithmetic/migration controls. Current-window
    // behavior is exercised separately in receipt_tests, never by relabeling
    // these local-JSON scenarios as provider observations.
    fn legacy_trajectory() -> Trajectory {
        Trajectory {
            receipts: None,
            ..Trajectory::default()
        }
    }

    fn request(text: &str) -> Value {
        json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": text}], "prompt_cache_key": "scenario"})
    }

    fn response(input: u64) -> Value {
        json!({"status": "completed", "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]}],
            "usage": {"input_tokens": input, "output_tokens": 3, "input_tokens_details": {"cached_tokens": 0}}})
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
                let body = request(&"evidence ".repeat(2000));
                let mut terminal = response(100);
                terminal["output"] = json!([{"type": kind, "call_id": "call_1"}]);
                let total = projection(&body, "route", 100).unwrap().input
                    + token_units(&terminal["output"]);
                let mut trajectory = Trajectory {
                    scenario: Scenario {
                        context_window: total + 100 - u64::from(crossing),
                        reserve: 100,
                        keep_recent: 1500,
                        summary: 100,
                    },
                    ..legacy_trajectory()
                };
                trajectory.primary(&body, &terminal, "route", 100, true);
                let before = trajectory.snapshot();
                assert!(before.priced);
                assert_eq!(
                    before.traditional_compactions,
                    u64::from(crossing),
                    "{kind}"
                );
                assert_eq!(before.summary_cost > 0.0, crossing, "{kind}");
                if !crossing {
                    continue;
                }
                assert_eq!(trajectory.history.last_traditional_input, 1600);
                assert!(trajectory.traditional_edited);
                let mut resumed: Trajectory =
                    serde_json::from_value(serde_json::to_value(&trajectory).unwrap()).unwrap();
                let mut next = body.clone();
                next["input"]
                    .as_array_mut()
                    .unwrap()
                    .extend(terminal["output"].as_array().unwrap().iter().cloned());
                next["input"].as_array_mut().unwrap().push(
                    json!({"type": "function_call_output", "call_id": "call_1", "output": "ok"}),
                );
                let next_input = (projection(&next, "route", 101).unwrap().input as i128
                    - resumed.history.retired as i128
                    + resumed.history.summary as i128) as u64;
                let expected = scenario_input_cost("gpt-6-luna", next_input, 0, Some(1024))
                    .unwrap()
                    + crate::savings::scenario_output_cost("gpt-6-luna", next_input, 3).unwrap();
                resumed.primary(&next, &response(100), "route", 101, true);
                let after = resumed.snapshot();
                assert_eq!(
                    after.traditional_compactions, 1,
                    "{kind}: no duplicate summary"
                );
                assert_eq!(after.summary_cost, before.summary_cost);
                assert!(
                    (after.traditional_cost - before.traditional_cost - expected).abs() < 1e-12,
                    "{kind}: the first post-summary request must be cold after restart"
                );
            }
        }
    }

    #[test]
    fn priced_subset_omits_primary_atomically_and_resumes_after_restart() {
        for variant in [
            "missing_usage",
            "unknown_model",
            "unknown_cache",
            "image",
            "audio",
            "missing_output",
            "nonstandard",
        ] {
            let mut trajectory = legacy_trajectory();
            let body = request(&"prefix ".repeat(3000));
            trajectory.primary(&body, &response(100), "route", 100, true);
            trajectory.reviewer(&request("known review"), &response(100), true);
            let before = trajectory.snapshot();
            let mut omitted = body.clone();
            let mut terminal = response(100);
            match variant {
                "missing_usage" => {
                    terminal.as_object_mut().unwrap().remove("usage");
                }
                "unknown_model" => omitted["model"] = json!("unknown-model"),
                "unknown_cache" => omitted["model"] = json!("gpt-6-astra"),
                "image" => {
                    omitted["input"][0]["content"] =
                        json!([{"type":"input_image", "image_url":"opaque"}])
                }
                "audio" => terminal["usage"]["output_tokens_details"]["audio_tokens"] = json!(1),
                "missing_output" => {
                    terminal.as_object_mut().unwrap().remove("output");
                }
                _ => (),
            }
            trajectory.primary(&omitted, &terminal, "route", 101, variant != "nonstandard");
            let partial = trajectory.snapshot();
            assert!(partial.priced, "{variant}: retain the known matched prefix");
            assert_eq!(
                partial.carry_cost, before.carry_cost,
                "{variant}: no half-pair Carry charge"
            );
            assert_eq!(
                partial.traditional_cost, before.traditional_cost,
                "{variant}: no half-pair traditional charge"
            );
            let public = serde_json::to_value(partial.dashboard(false)).unwrap();
            assert_eq!(public["excluded_unpriced_calls"], 1, "{variant}");
            assert_eq!(public["excluded_unpriced_reviewer_calls"], 0);
            assert!(
                public["savings"].as_f64().unwrap() < 0.0,
                "known reviewer overhead stays real"
            );
            assert!(
                trajectory.previous.is_none(),
                "{variant}: omitted primary invalidates the cache predecessor"
            );
            let mut resumed: Trajectory =
                serde_json::from_value(serde_json::to_value(&trajectory).unwrap()).unwrap();
            let mut cold = legacy_trajectory();
            cold.primary(&body, &response(100), "route", 102, true);
            resumed.primary(&body, &response(100), "route", 102, true);
            let after = resumed.snapshot();
            assert!(
                after.priced,
                "{variant}: a later priced primary remains comparable"
            );
            assert!(
                (after.carry_cost - before.carry_cost - cold.snapshot().carry_cost).abs() < 1e-12,
                "{variant}: do not reuse a stale cache across the omission"
            );
            assert_eq!(
                serde_json::to_value(after).unwrap()["excluded_unpriced_calls"],
                1
            );
        }
    }

    #[test]
    fn persisted_reviewer_expense_roundtrips_without_repricing() {
        // This exact paid total exposed a one-ULP drift at a real restart.
        // Exercise the checkpoint codec, not only a Value-to-Value conversion.
        for expense in [0.00011470000000000001_f64, 0.0001147, 0.0] {
            let mut trajectory = legacy_trajectory();
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
    fn priced_subset_unpriced_only_is_null_even_with_known_reviewer_expense() {
        for variant in ["missing_usage", "unknown_model", "unknown_cache", "native"] {
            let mut trajectory = legacy_trajectory();
            let mut body = request("task");
            let mut terminal = response(100);
            match variant {
                "missing_usage" => {
                    terminal.as_object_mut().unwrap().remove("usage");
                }
                "unknown_model" => body["model"] = json!("unknown-model"),
                "unknown_cache" => body["model"] = json!("gpt-6-astra"),
                _ => {
                    body["input"][0]["content"] =
                        json!([{"type":"input_image", "image_url":"opaque"}])
                }
            }
            trajectory.primary(&body, &terminal, "route", 100, true);
            trajectory.reviewer(&request("known review"), &response(100), true);
            let public = serde_json::to_value(trajectory.snapshot().dashboard(false)).unwrap();
            assert!(
                public["carry_cost"].is_null(),
                "{variant}: no comparable primary, not a false zero"
            );
            assert_eq!(public["excluded_unpriced_calls"], 1);
        }
    }

    #[test]
    fn priced_subset_reviewer_omissions_preserve_known_expense_without_touching_primary_cache() {
        let mut trajectory = legacy_trajectory();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        trajectory.reviewer(&request("known review"), &response(100), true);
        let before = serde_json::to_value(&trajectory).unwrap();
        trajectory.reviewer_usage("unknown-model", 100, &response(100)["usage"], true);
        trajectory.reviewer_usage("gpt-6-luna", 100, &Value::Null, true);
        let after = serde_json::to_value(&trajectory).unwrap();
        assert_eq!(after["previous"], before["previous"]);
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
        let mut trajectory = legacy_trajectory();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        trajectory.reviewer_usage("unknown-model", 100, &response(100)["usage"], true);
        let mut totals = DashboardTotals::default();
        totals.observe(&trajectory.snapshot().dashboard(false));
        totals.observe(&trajectory.snapshot().dashboard(true));
        trajectory.prior_untracked(1);
        totals.observe(&trajectory.snapshot().dashboard(false));
        let public = serde_json::to_value(totals).unwrap();
        assert_eq!(public["included_sessions"], 0);
        assert_eq!(public["excluded_sessions"], 3);
        assert_eq!(public["legacy_sessions"], 3);
        assert_eq!(public["excluded_unpriced_calls"], 3);
        assert_eq!(public["excluded_unpriced_reviewer_calls"], 3);
        assert!(public["carry_cost"].is_null());
    }

    #[test]
    fn priced_subset_later_summaries_keep_advancing_through_omissions() {
        for variant in ["missing_usage", "unknown_model", "image"] {
            let mut trajectory = Trajectory {
                scenario: Scenario {
                    context_window: 2000,
                    reserve: 100,
                    keep_recent: 200,
                    summary: 50,
                },
                ..legacy_trajectory()
            };
            trajectory.primary(&request("task"), &response(100), "route", 100, true);
            let mut body = request("omitted");
            let mut terminal = response(100);
            match variant {
                "missing_usage" => {
                    terminal.as_object_mut().unwrap().remove("usage");
                }
                "unknown_model" => body["model"] = json!("unknown-model"),
                _ => {
                    body["input"][0]["content"] =
                        json!([{"type":"input_image", "image_url":"opaque"}])
                }
            }
            trajectory.primary(&body, &terminal, "route", 101, true);
            let mut resumed: Trajectory =
                serde_json::from_value(serde_json::to_value(&trajectory).unwrap()).unwrap();
            let large = request(&"evidence ".repeat(3000));
            resumed.primary(&large, &response(100), "route", 102, true);
            let summary = resumed.snapshot();
            assert!(summary.priced, "{variant}");
            assert_eq!(
                summary.traditional_compactions, 1,
                "{variant}: summaries must not stop forever"
            );
            assert!(summary.summary_cost > 0.0);
            resumed.primary(&large, &response(100), "route", 103, true);
            assert_eq!(resumed.snapshot().traditional_compactions, 1);
            assert_eq!(resumed.snapshot().summary_cost, summary.summary_cost);
        }
    }

    #[test]
    fn priced_subset_omitted_threshold_crossing_updates_summary_structure_not_money() {
        let mut trajectory = Trajectory {
            scenario: Scenario {
                context_window: 2000,
                reserve: 100,
                keep_recent: 200,
                summary: 50,
            },
            ..legacy_trajectory()
        };
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        let before = trajectory.snapshot();
        let large = request(&"evidence ".repeat(3000));
        trajectory.primary(
            &large,
            &json!({"status":"completed","output":[]}),
            "route",
            101,
            true,
        );
        assert_eq!(trajectory.snapshot().carry_cost, before.carry_cost);
        assert_eq!(
            trajectory.snapshot().traditional_cost,
            before.traditional_cost
        );
        assert_eq!(trajectory.snapshot().traditional_compactions, 1);
        trajectory.primary(&large, &response(100), "route", 102, true);
        assert_eq!(
            trajectory.snapshot().traditional_compactions,
            1,
            "do not invent a delayed duplicate summary charge"
        );
        assert_eq!(
            trajectory.snapshot().summary_cost,
            0.0,
            "omitted boundary's summary is excluded with its call"
        );
        assert!(trajectory.snapshot().priced);
    }

    #[test]
    fn priced_subset_legacy_poisoned_sums_remain_unavailable_without_replay() {
        let mut trajectory = legacy_trajectory();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        let mut checkpoint = serde_json::to_value(trajectory).unwrap();
        checkpoint
            .as_object_mut()
            .unwrap()
            .remove("priced_primary_responses");
        checkpoint["available"] = json!(false);
        checkpoint["result"]["coverage"]["unmodeled_calls"] = json!(1);
        checkpoint["result"]
            .as_object_mut()
            .unwrap()
            .remove("excluded_unpriced_calls");
        checkpoint["result"]
            .as_object_mut()
            .unwrap()
            .remove("excluded_unpriced_reviewer_calls");
        let mut resumed: Trajectory = serde_json::from_value(checkpoint).unwrap();
        resumed.primary(&request("later task"), &response(100), "route", 101, true);
        assert!(
            !resumed.snapshot().priced,
            "old accounting may have skipped conventional summaries; never flip available"
        );
        let public = serde_json::to_value(resumed.snapshot().dashboard(false)).unwrap();
        assert_eq!(
            public["excluded_unpriced_calls"], 1,
            "historical omissions stay visible even when sums cannot be repaired"
        );
    }

    #[test]
    fn legacy_overlapping_omission_reasons_remain_a_lower_bound_across_restart() {
        for (extra_unmodeled, keep_explicit_count, expected) in
            [(0, false, 1), (2, false, 2), (2, true, 3)]
        {
            let mut trajectory = legacy_trajectory();
            let mut body = request("one request with overlapping omission reasons");
            body["model"] = json!("gpt-6-astra");
            trajectory.primary(
                &body,
                &json!({"status": "completed", "output": []}),
                "route",
                100,
                true,
            );
            let single = trajectory.snapshot();
            assert_eq!(single.coverage.primary_responses, 1);
            assert_eq!(single.coverage.missing_usage, 1);
            assert_eq!(single.coverage.unknown_cache_requests, 1);
            assert_eq!(single.excluded_unpriced_calls, 1);
            for _ in 0..extra_unmodeled {
                trajectory.unmodeled_call();
            }
            assert_eq!(
                trajectory.snapshot().excluded_unpriced_calls,
                1 + extra_unmodeled
            );

            // Reproduce the legacy checkpoint shape, not a second accounting path.
            let mut checkpoint = serde_json::to_value(trajectory).unwrap();
            let object = checkpoint.as_object_mut().unwrap();
            object.remove("priced_primary_responses");
            object.remove("successful_requests_only");
            checkpoint["available"] = json!(false);
            if !keep_explicit_count {
                checkpoint["result"]
                    .as_object_mut()
                    .unwrap()
                    .remove("excluded_unpriced_calls");
            }
            checkpoint["result"]
                .as_object_mut()
                .unwrap()
                .remove("excluded_unpriced_reviewer_calls");
            let mut legacy: Trajectory = serde_json::from_value(checkpoint).unwrap();
            let projected = serde_json::to_value(legacy.snapshot().dashboard(false)).unwrap();
            assert_eq!(projected["excluded_unpriced_calls"], expected);
            assert_eq!(projected["excluded_unpriced_reviewer_calls"], 0);
            assert!(projected["carry_cost"].is_null());
            assert_eq!(legacy.priced_primary_responses, None);

            // A later observation migrates the lower bound without repairing old sums.
            legacy.primary(&request("later task"), &response(100), "route", 101, true);
            assert_eq!(legacy.priced_primary_responses, Some(1));
            assert_eq!(legacy.result.excluded_unpriced_calls, expected);
            let mut resumed: Trajectory =
                serde_json::from_value(serde_json::to_value(legacy).unwrap()).unwrap();
            assert_eq!(resumed.snapshot().excluded_unpriced_calls, expected);
            assert!(!resumed.snapshot().priced);
            resumed.primary(
                &request("new omission after migration"),
                &json!({"status": "completed", "output": []}),
                "route",
                102,
                true,
            );
            let restarted: Trajectory =
                serde_json::from_value(serde_json::to_value(resumed).unwrap()).unwrap();
            let dashboard = restarted.snapshot().dashboard(false);
            let public = serde_json::to_value(&dashboard).unwrap();
            assert_eq!(public["excluded_unpriced_calls"], expected + 1);
            assert_eq!(public["excluded_unpriced_reviewer_calls"], 0);
            assert!(public["carry_cost"].is_null());

            let mut clean = legacy_trajectory();
            clean.primary(
                &request("priced control"),
                &response(100),
                "route",
                100,
                true,
            );
            let mut totals = DashboardTotals::default();
            totals.observe(&clean.snapshot().dashboard(false));
            totals.observe(&dashboard);
            let aggregate = serde_json::to_value(totals).unwrap();
            assert_eq!(aggregate["excluded_unpriced_calls"], expected + 1);
            assert_eq!(aggregate["excluded_unpriced_reviewer_calls"], 0);
            assert_eq!(aggregate["included_sessions"], 0);
            assert_eq!(aggregate["excluded_sessions"], 2);
            assert_eq!(aggregate["legacy_sessions"], 2);
            assert!(aggregate["carry_cost"].is_null());
        }
    }

    #[test]
    fn dashboard_projection_keeps_completed_money_without_changing_persisted_estimate() {
        let mut trajectory = legacy_trajectory();
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
    fn successful_requests_continue_summaries_after_failures_and_restart() {
        let mut trajectory = Trajectory {
            scenario: Scenario {
                context_window: 2000,
                reserve: 100,
                keep_recent: 200,
                summary: 50,
            },
            ..legacy_trajectory()
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
        let mut trajectory = legacy_trajectory();
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
        let mut trajectory = legacy_trajectory();
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
    fn legacy_checkpoints_keep_fail_closed_policy_across_epoch_and_restart() {
        let mut initial = legacy_trajectory();
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
            let mut trajectory = legacy_trajectory();
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
    fn scenario_input_is_independent_of_native_input_metering() {
        let replay = |metered| {
            let mut trajectory = legacy_trajectory();
            trajectory.primary(
                &request(&"evidence ".repeat(2000)),
                &response(metered),
                "route",
                100,
                true,
            );
            trajectory.projected(1000, 0, true);
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
        let mut trajectory = legacy_trajectory();
        trajectory.projected(20, 30, true);
        trajectory.primary(&body, &response(100), "route", 100, true);
        let expected = projection(&body, "route", 100).unwrap().input - 10;
        assert_eq!(
            trajectory.history.last_traditional_input, expected,
            "net context growth must not inflate the traditional baseline"
        );
    }

    #[test]
    fn reviewer_overhead_can_produce_a_negative_saving() {
        let mut trajectory = legacy_trajectory();
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
    fn conventional_summary_occurs_after_a_tool_response_and_survives_restart() {
        let mut trajectory = Trajectory {
            scenario: Scenario {
                context_window: 2000,
                reserve: 100,
                keep_recent: 200,
                summary: 50,
            },
            ..legacy_trajectory()
        };
        let body = request(&"evidence ".repeat(3000));
        let mut tool_response = response(3000);
        tool_response["output"] =
            json!([{"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"}]);
        trajectory.primary(&body, &tool_response, "route", 100, true);
        assert_eq!(trajectory.snapshot().traditional_compactions, 1);
        let first_summary = trajectory.snapshot().summary_cost;
        trajectory.primary(&body, &response(3000), "route", 101, true);
        let before = trajectory.snapshot();
        assert_eq!(before.traditional_compactions, 1);
        assert_eq!(before.summary_cost, first_summary);
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
        let mut initial = legacy_trajectory();
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
        let mut trajectory = legacy_trajectory();
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
                    ..legacy_trajectory()
                };
                // Establish the alternative size through the real compaction path.
                if traditional_input >= carry_input {
                    trajectory.projected(traditional_input - carry_input, 0, true);
                } else {
                    trajectory.projected(0, carry_input - traditional_input, true);
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
        let mut trajectory = legacy_trajectory();
        trajectory.primary(&body, &response(100), "route", 100, true);
        let estimate = trajectory.snapshot();
        assert_eq!(
            estimate.carry_cost, 0.0,
            "unknown-cache primary is omitted atomically"
        );
        assert_eq!(estimate.traditional_cost, 0.0);
        assert_eq!(estimate.coverage.unknown_cache_requests, 1);
        assert!(!estimate.priced);
        assert_eq!(estimate.savings_percent, None);
    }

    #[test]
    fn restored_unknown_cache_coverage_excludes_scenario_savings() {
        let mut trajectory = legacy_trajectory();
        trajectory.primary(&request("task"), &response(100), "route", 100, true);
        let mut checkpoint = serde_json::to_value(&trajectory).unwrap();
        checkpoint
            .as_object_mut()
            .unwrap()
            .remove("priced_primary_responses");
        let mut clean_legacy: Trajectory = serde_json::from_value(checkpoint.clone()).unwrap();
        assert!(clean_legacy.snapshot().priced);
        clean_legacy.primary(&request("later task"), &response(100), "route", 101, true);
        assert!(clean_legacy.snapshot().priced);
        assert_eq!(clean_legacy.priced_primary_responses, Some(2));
        checkpoint["result"]["coverage"]["unknown_cache_requests"] = json!(1);
        let mut restored: Trajectory = serde_json::from_value(checkpoint).unwrap();
        assert!(!restored.snapshot().priced);
        assert_eq!(restored.snapshot().savings_percent, None);
        restored.primary(&request("later task"), &response(100), "route", 101, true);
        assert!(
            !restored.snapshot().priced,
            "new calls cannot repair old unknown-cache sums"
        );
    }

    #[test]
    fn native_multimodal_inputs_are_not_priced_as_serialized_text() {
        for content in [
            json!([{"type": "input_image", "image_url": "opaque"}]),
            json!([{"type": "input_audio", "data": "opaque"}]),
        ] {
            let mut body = request("task");
            body["input"][0]["content"] = content;
            let mut trajectory = legacy_trajectory();
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
        let mut checkpoint = serde_json::to_value(legacy_trajectory()).unwrap();
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
            let unavailable = variant == 1 || variant == 2;
            assert_eq!(trajectory.snapshot().priced, !unavailable);
            assert_eq!(trajectory.snapshot().savings_percent.is_none(), unavailable);
        }
    }
}
