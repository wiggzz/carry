//! Traditional quantities and continuity, independent of cache and money.
use super::*;
use sha2::{Digest, Sha256};

/// These are different units, even when a synthetic fixture gives them the same
/// numeric value. Changing either adapter requires a new history contract.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
enum QuantityBasis {
    ProviderRequestInputTotalV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
enum ProjectionBasis {
    SignedLocalPromptRestorationV1,
    ReceiptBlocksOrVisibleTextV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct HistoryContract {
    version: u32,
    quantity: QuantityBasis,
    projection: ProjectionBasis,
    model: String,
    resolved_model: Option<String>,
    source_sha256: String,
    source_class: Option<carry::models::RouteClass>,
    prompt_settings_sha256: String,
    keep_recent: u64,
    summary_tokens: u64,
    initialization_version: u32,
}
impl HistoryContract {
    fn compatible(&self, other: &Self) -> bool {
        // Off mode omits resolved metadata; that absence is not evidence of a
        // new canonical history. Exact model/body/source hashes still fence
        // continuity, and contradictory KNOWN resolution facts still reject.
        let mut prior = self.clone();
        let mut next = other.clone();
        prior.resolved_model = prior.resolved_model.or_else(|| next.resolved_model.clone());
        next.resolved_model = next.resolved_model.or_else(|| prior.resolved_model.clone());
        prior.source_class = prior.source_class.or_else(|| next.source_class.clone());
        next.source_class = next.source_class.or_else(|| prior.source_class.clone());
        prior == next
    }

    pub(super) fn extract(
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
        scenario: Scenario,
    ) -> Self {
        Self {
            version: 1,
            quantity: QuantityBasis::ProviderRequestInputTotalV1,
            projection: ProjectionBasis::ReceiptBlocksOrVisibleTextV2,
            model: body["model"].as_str().unwrap_or("").into(),
            resolved_model: resolved.map(|r| r.model.clone()),
            source_sha256: format!("{:x}", Sha256::digest(route.as_bytes())),
            source_class: resolved.map(|r| r.route.class.clone()),
            prompt_settings_sha256: format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&crate::savings::prompt_overhead(body)).unwrap())
            ),
            keep_recent: scenario.keep_recent,
            summary_tokens: scenario.summary,
            initialization_version: 1,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct NativeInputCursor {
    basis: InputBasis,
    prefix: String,
    items: usize,
    input: u64,
    at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
enum InputBasis {
    SemanticV1,
}
impl NativeInputCursor {
    fn prefix(items: &[Value]) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&crate::openai::semantic_prompt_input(items)).unwrap()
            )
        )
    }
    fn matches(&self, body: &Value, input: u64, at: u64, projected: bool) -> bool {
        at >= self.at
            && (projected
                || body["input"].as_array().is_some_and(|items| {
                    items.len() >= self.items
                        && Self::prefix(&items[..self.items]) == self.prefix
                        && input >= self.input
                        && (items.len() > self.items || input == self.input)
                }))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct HistoryContinuity {
    contract: HistoryContract,
    cursor: NativeInputCursor,
}

/// A response whose retained quantity is unknown has not yet decided whether
/// the alternative summarized. Only the exact next semantic input can isolate
/// its native retained increment; extra caller appends remain ambiguous.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct PendingRetention {
    prefix: String,
    items: usize,
    restoration: String,
    pub(super) fixed: u64,
    visible: Option<u64>,
    opaque_items: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProjectionRecovery {
    configuration: String,
    cursor: NativeInputCursor,
}

/// Kept in the existing atomic trajectory. Cache eligibility and monetary
/// interpretation have no mutable access through this API.
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
pub(super) struct TraditionalHistoryState {
    // Proof for the entire comparison history, not a last-plan/no-op flag.
    // Missing proof never opts into sharing a provider observation.
    #[serde(default)]
    shared_primary_history: crate::savings::PrimaryHistory,
    pub(super) retired: u64,
    pub(super) summary: u64,
    pub(super) last_traditional_input: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) history_continuity: Option<HistoryContinuity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pending_retention: Option<PendingRetention>,
    #[serde(default)]
    pub(super) unresolved_projection: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projection_recovery: Option<ProjectionRecovery>,
}
impl TraditionalHistoryState {
    pub(super) fn defer_projection(
        &mut self,
        configuration: String,
        body: &Value,
        response: &Value,
        input: Option<u64>,
        at: u64,
    ) {
        self.projection_recovery = if !self.unresolved_projection
            && response["output"].as_array().is_some_and(Vec::is_empty)
        {
            input
                .zip(body["input"].as_array())
                .map(|(input, items)| ProjectionRecovery {
                    configuration,
                    cursor: NativeInputCursor {
                        basis: InputBasis::SemanticV1,
                        prefix: NativeInputCursor::prefix(items),
                        items: items.len(),
                        input,
                        at,
                    },
                })
        } else {
            None
        };
        self.unresolved_projection = true;
    }

    pub(super) fn recover_projection(
        &mut self,
        configuration: &str,
        body: &Value,
        input: u64,
        at: u64,
    ) -> Option<u64> {
        let prior = self.projection_recovery.as_ref()?;
        let items = body["input"].as_array()?;
        if prior.configuration != configuration
            || input != prior.cursor.input
            || at < prior.cursor.at
            || items.len() != prior.cursor.items
            || NativeInputCursor::prefix(items) != prior.cursor.prefix
        {
            return None;
        }
        let at = prior.cursor.at;
        self.unresolved_projection = false;
        self.projection_recovery = None;
        Some(at)
    }

    pub(super) fn defer_retention(
        &mut self,
        body: &Value,
        output: &Value,
        restoration: i128,
        fixed: u64,
    ) {
        let mut next = body["input"].as_array().unwrap().clone();
        let output = output.as_array();
        if let Some(output) = output {
            next.extend(output.iter().cloned());
        }
        self.pending_retention = Some(PendingRetention {
            prefix: output.map_or_else(String::new, |_| NativeInputCursor::prefix(&next)),
            items: next.len(),
            restoration: restoration.to_string(),
            fixed,
            visible: receipts::retained_output_tokens(&Value::Array(
                output.cloned().unwrap_or_default(),
            )),
            opaque_items: output.and_then(|output| {
                crate::proxy_quantity::retained_reasoning_count(&Value::Array(output.clone()))
            }),
        });
    }

    pub(super) fn resolve_retention(
        &self,
        body: &Value,
        native: u64,
        restoration: i128,
        projected: bool,
        scenario: Scenario,
        measurements: Option<&crate::proxy_quantity::InputMeasurements>,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<(u64, u64)> {
        let pending = self.pending_retention.as_ref()?;
        let cursor = &self.history_continuity.as_ref()?.cursor;
        let items = body["input"].as_array()?;
        if projected
            || restoration.to_string() != pending.restoration
            || items.len() < pending.items
            || NativeInputCursor::prefix(&items[..pending.items]) != pending.prefix
        {
            return None;
        }
        let upper = native.checked_sub(cursor.input)?;
        let retained = if items.len() == pending.items
            || scenario
                .response_compaction(self.last_traditional_input, upper, pending.fixed)
                .ok()
                == Some(None)
        {
            // Extra appends have nonnegative input quantities. Even without an
            // allocation, their whole native increment can PROVE no intervening
            // summary. This bound is never reported as an output measurement.
            upper
        } else {
            // Extra caller input prevents exact attribution. Admit the same
            // neutral residual estimator only when this output's member slice
            // has valid attribution; never bypass a rejected native block.
            let m = measurements?;
            let mut without_output = body.clone();
            without_output["input"]
                .as_array_mut()?
                .drain(cursor.items..pending.items);
            m.clone()
                .accounting_restoration(body, &without_output, route, resolved)?;
            pending
                .visible?
                .checked_add(m.retained_reasoning_units(pending.opaque_items?)?)?
        };
        Some((retained, pending.fixed))
    }

    pub(super) fn identical() -> Self {
        Self {
            shared_primary_history: crate::savings::PrimaryHistory::identical(),
            ..Self::default()
        }
    }

    pub(super) fn price_primary(
        &self,
        observed: Option<f64>,
        forecast: impl FnOnce() -> Option<f64>,
    ) -> Option<(f64, f64)> {
        self.shared_primary_history.price(observed, forecast)
    }

    pub(super) fn diverge(&mut self) {
        self.shared_primary_history.diverge();
    }

    pub(super) fn is_identical(&self) -> bool {
        self.shared_primary_history.is_identical()
    }

    pub(super) fn compatible(
        &self,
        contract: &HistoryContract,
        body: &Value,
        input: u64,
        at: u64,
        projected: bool,
    ) -> Option<bool> {
        self.history_continuity.as_ref().map(|prior| {
            prior.contract.compatible(contract) && prior.cursor.matches(body, input, at, projected)
        })
    }

    pub(super) fn input(&self, native: u64, restoration: i128) -> u64 {
        (native as i128)
            .saturating_add(restoration)
            .saturating_sub(self.retired as i128)
            .saturating_add(self.summary as i128)
            .clamp(0, u64::MAX as i128) as u64
    }

    pub(super) fn summarize(&mut self, total: u64, kept: u64, summary: u64) {
        self.diverge();
        self.retired = self
            .retired
            .saturating_add(total.saturating_sub(kept).saturating_sub(self.summary));
        self.summary = summary;
        self.last_traditional_input = kept.saturating_add(summary);
    }

    pub(super) fn observe(
        &mut self,
        mut contract: HistoryContract,
        items: &[Value],
        input: u64,
        at: u64,
    ) {
        if let Some(previous) = &self.history_continuity {
            if previous.contract.compatible(&contract) {
                contract.resolved_model = contract
                    .resolved_model
                    .or_else(|| previous.contract.resolved_model.clone());
                contract.source_class = contract
                    .source_class
                    .or_else(|| previous.contract.source_class.clone());
            }
        }
        self.history_continuity = Some(HistoryContinuity {
            contract,
            cursor: NativeInputCursor {
                basis: InputBasis::SemanticV1,
                prefix: NativeInputCursor::prefix(items),
                items: items.len(),
                input,
                at,
            },
        });
    }
}
