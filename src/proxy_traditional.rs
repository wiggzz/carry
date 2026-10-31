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
    pub(super) fn extract(
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
        scenario: Scenario,
    ) -> Self {
        Self {
            version: 1,
            quantity: QuantityBasis::ProviderRequestInputTotalV1,
            projection: ProjectionBasis::SignedLocalPromptRestorationV1,
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
    #[serde(default)]
    basis: InputBasis,
    prefix: String,
    items: usize,
    input: u64,
    at: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
enum InputBasis {
    #[default]
    RawV1,
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

/// Kept in the existing atomic trajectory, with the old numeric field names for
/// persisted readers. Cache eligibility and monetary interpretation have no
/// mutable access through this API. The legacy decoder is a separate boundary.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct TraditionalHistoryState {
    // Proof for the entire comparison history, not a last-plan/no-op flag.
    // Missing legacy proof never opts into sharing a provider observation.
    #[serde(default)]
    shared_primary_history: crate::savings::PrimaryHistory,
    pub(super) retired: u64,
    pub(super) summary: u64,
    pub(super) last_traditional_input: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) history_continuity: Option<HistoryContinuity>,
    // Missing only in old checkpoints. The input-bound load adapter classifies
    // those once, before reserving a paid request; settlement never reconstructs
    // opaque history contracts or promotes unknown legacy history to a new one.
    #[serde(default)]
    pub(super) receipt_history_load: Option<ReceiptHistoryLoad>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) enum ReceiptHistoryLoad {
    StructuredV1,
    ReadOnlyLegacyV1,
}
impl Default for TraditionalHistoryState {
    fn default() -> Self {
        Self {
            shared_primary_history: Default::default(),
            retired: 0,
            summary: 0,
            last_traditional_input: 0,
            history_continuity: None,
            receipt_history_load: Some(ReceiptHistoryLoad::StructuredV1),
        }
    }
}
impl TraditionalHistoryState {
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

    pub(super) fn read_only_legacy(&self) -> bool {
        matches!(
            self.receipt_history_load,
            Some(ReceiptHistoryLoad::ReadOnlyLegacyV1)
        )
    }

    /// One-time input-bound conversion of the old opaque raw-item cursor.
    /// Only the exact prior raw prefix can prove which semantic input it named.
    /// An unprovable prefix stays frozen/read-only, never a fresh history.
    pub(super) fn load_input_cursor(&mut self, body: &Value) {
        if self.read_only_legacy() {
            return;
        }
        let Some(prior) = &mut self.history_continuity else {
            return;
        };
        if prior.cursor.basis != InputBasis::RawV1 {
            return;
        }
        let prefix = body["input"]
            .as_array()
            .and_then(|items| items.get(..prior.cursor.items));
        if let Some(prefix) = prefix
            && format!("{:x}", Sha256::digest(serde_json::to_vec(prefix).unwrap()))
                == prior.cursor.prefix
        {
            prior.cursor.prefix = NativeInputCursor::prefix(prefix);
            prior.cursor.basis = InputBasis::SemanticV1;
            // Only the cursor representation changed. Preserve separately saved
            // lifetime equality; a missing or false value remains unproven.
        } else {
            self.receipt_history_load = Some(ReceiptHistoryLoad::ReadOnlyLegacyV1);
        }
    }

    pub(super) fn load_legacy_cursor(
        &mut self,
        contract: HistoryContract,
        prefix: String,
        items: usize,
        input: u64,
        at: u64,
    ) {
        self.history_continuity = Some(HistoryContinuity {
            contract,
            cursor: NativeInputCursor {
                basis: InputBasis::RawV1,
                prefix,
                items,
                input,
                at,
            },
        });
        // The legacy cursor proves continuity, not lifetime identity of both
        // branches and not a provider write. Never manufacture sharing proof.
        self.diverge();
        self.receipt_history_load = Some(ReceiptHistoryLoad::StructuredV1);
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
            &prior.contract == contract && prior.cursor.matches(body, input, at, projected)
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
        contract: HistoryContract,
        items: &[Value],
        input: u64,
        at: u64,
    ) {
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
