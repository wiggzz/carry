//! One model-client receipt for either role. No transcript or second ledger.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Forecast {
    pub version: u32,
    pub input_tokens: Option<u64>,
    pub basis: String,
    pub at: u64,
    pub route: String,
    pub configuration: String,
    pub opaque: bool,
}

/// Frozen before transport, joined with native facts only after observation.
/// Stored in the existing bounded numeric evidence journal for both roles.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ClientReceipt {
    pub request_id: String,
    pub transport_ordinal: u8,
    pub parent_request_id: Option<String>,
    pub role: String,
    pub model: String,
    pub route: String,
    pub resolved: Option<ResolvedModel>,
    pub forecast: Forecast,
    pub provider_status: Option<String>,
    pub transport: String,
    pub http_status: Option<u16>,
    pub downstream_disconnected: bool,
    pub latency_ms: u64,
    pub error: Value,
    pub usage: Value,
    pub rates: Option<carry::core::Rates>,
    pub standard: bool,
    pub requested_tier: Option<String>,
    pub service_tier: Option<String>,
    pub write_tariff_known: bool,
    pub cost: Option<f64>,
    pub residual: Option<Residual>,
}

/// Exact signed u64 difference, JSON-safe even outside the i64 domain.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub(crate) struct Residual {
    pub negative: bool,
    pub absolute_tokens: u64,
}
impl Residual {
    fn between(actual: u64, forecast: u64) -> Self {
        Self {
            negative: actual < forecast,
            absolute_tokens: actual.abs_diff(forecast),
        }
    }
    fn signed(&self) -> f64 {
        self.absolute_tokens as f64 * if self.negative { -1.0 } else { 1.0 }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ErrorMoments {
    pub samples: u64,
    pub unavailable: u64,
    pub opaque_samples: u64,
    pub bias_tokens: f64,
    pub mean_absolute_error_tokens: f64,
    /// Population spread of residuals, not an uncertainty bound on native input.
    pub variance_tokens: f64,
}
impl ErrorMoments {
    fn observe(&mut self, receipt: &ClientReceipt) {
        let Some(error) = receipt
            .residual
            .as_ref()
            .filter(|_| receipt.forecast.version == 1)
        else {
            self.unavailable = self.unavailable.saturating_add(1);
            return;
        };
        let error = error.signed();
        let prior = self.samples as f64;
        self.samples = self.samples.saturating_add(1);
        self.opaque_samples = self
            .opaque_samples
            .saturating_add(u64::from(receipt.forecast.opaque));
        let n = self.samples as f64;
        let delta = error - self.bias_tokens;
        self.bias_tokens += delta / n;
        self.variance_tokens =
            (prior * self.variance_tokens + delta * (error - self.bias_tokens)) / n;
        self.mean_absolute_error_tokens += (error.abs() - self.mean_absolute_error_tokens) / n;
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Calibration {
    pub primary: ErrorMoments,
    pub reviewer: ErrorMoments,
}
impl Calibration {
    pub(crate) fn observe(&mut self, receipt: &ClientReceipt) {
        if receipt.role == "reviewer" {
            self.reviewer.observe(receipt);
        } else {
            self.primary.observe(receipt);
        }
    }
}

impl ClientReceipt {
    fn tier(value: &Value) -> Option<String> {
        value.get("service_tier").map(|v| match v.as_str() {
            Some(s @ ("default" | "auto" | "flex" | "priority" | "scale")) => s.into(),
            _ => "unknown".into(),
        })
    }
    /// Exact optional-field envelope for existing predispatch settlement admission.
    /// Numeric widths are expanded by settlement::size_bound, not guessed bytes.
    pub(crate) fn shape(body: &Value, resolved: Option<&ResolvedModel>) -> Self {
        serde_json::from_value(json!({
            "request_id":"0".repeat(80), "transport_ordinal":0, "parent_request_id":"0".repeat(80),
            "role":"reviewer", "model":body["model"].as_str().unwrap_or(""),
            "route":"0".repeat(64), "resolved":resolved,
            "forecast":{"version":1,"input_tokens":0,"basis":"0".repeat(128),"at":0,
                "route":"0".repeat(64),"configuration":"0".repeat(64),"opaque":false},
            "provider_status":"in_progress", "transport":"0".repeat(64),
            "http_status":0,"downstream_disconnected":false,"latency_ms":0,
            "error":{"code":"0".repeat(64),"type":"0".repeat(64),"param":"0".repeat(64)},
            "usage":crate::proxy_savings::review_usage_shape(),
            "rates":{"input":0,"cached":0,"write":0,"output":0},
            "standard":false,"requested_tier":"unknown","service_tier":"unknown",
            "write_tariff_known":false,"cost":0,"residual":{"negative":false,"absolute_tokens":0}
        }))
        .unwrap()
    }

    pub(super) fn dispatch(
        attempt: &Attempt,
        body: &Value,
        route: &str,
        measurement: Option<&crate::proxy_state::DispatchMeasurement>,
    ) -> Self {
        // Route identity is useful; userinfo/query credentials are not.
        let route = format!("{:x}", Sha256::digest(route.as_bytes()));
        Self {
            request_id: attempt.id.clone(),
            transport_ordinal: 0,
            parent_request_id: measurement.map(|m| m.request_id.clone()),
            role: if attempt.actor == "shadow" {
                "reviewer"
            } else {
                "primary"
            }
            .into(),
            model: attempt.model.clone(),
            route: route.clone(),
            resolved: attempt.resolved.clone(),
            forecast: Forecast {
                version: 1,
                input_tokens: measurement.and_then(|m| m.outgoing_input_tokens),
                basis: measurement
                    .map_or("unavailable", |m| m.basis.as_str())
                    .chars()
                    .take(128)
                    .collect(),
                at: measurement.map_or_else(now, |m| m.at),
                route: route.clone(),
                configuration: crate::proxy_savings::configuration(
                    body,
                    &route,
                    attempt.resolved.as_ref(),
                ),
                opaque: crate::savings::prompt_has_opaque(body),
            },
            provider_status: None,
            transport: "not_observed".into(),
            http_status: None,
            downstream_disconnected: false,
            latency_ms: 0,
            error: json!({}),
            usage: Value::Null,
            rates: None,
            standard: attempt.standard,
            requested_tier: Self::tier(body),
            service_tier: None,
            write_tariff_known: attempt.write_tariff_known,
            cost: None,
            residual: None,
        }
    }

    pub(super) fn observe(
        &mut self,
        response: Option<&Value>,
        outcome: &str,
        status: Option<u16>,
        downstream_disconnected: bool,
        elapsed: u64,
        write_tariff_known: bool,
    ) {
        self.transport = outcome.into();
        self.http_status = status;
        self.downstream_disconnected = downstream_disconnected;
        self.latency_ms = elapsed;
        if let Some(value) = response {
            // Never retain arbitrary provider strings, prose or headers.
            self.provider_status = value["status"].as_str().and_then(|s| {
                [
                    "completed",
                    "failed",
                    "incomplete",
                    "cancelled",
                    "in_progress",
                ]
                .contains(&s)
                .then(|| s.into())
            });
            self.error = crate::proxy_sse::safe_error(&value["error"]);
            self.usage = crate::proxy_savings::numeric_review_usage(&value["usage"]);
            self.service_tier = Self::tier(value);
            self.standard &= value.get("service_tier").is_none_or(|v| v == "default");
            self.rates = receipt_rates(
                &self.model,
                &self.usage,
                self.resolved.as_ref(),
                write_tariff_known,
            );
            self.residual = self
                .forecast
                .input_tokens
                .zip(self.usage["input_tokens"].as_u64())
                .map(|(forecast, actual)| Residual::between(actual, forecast));
        }
        let mut contribution = carry::core::UsageLedger::default();
        self.cost = self.settle(&mut contribution);
    }

    /// Only this client-layer owner interprets the frozen receipt for a ledger.
    /// Background review transfers this record, never today's settings or stale totals.
    pub(super) fn settle(&self, ledger: &mut carry::core::UsageLedger) -> Option<f64> {
        ledger.settle_with_rates(&self.model, &self.usage, self.standard, self.rates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receipt(role: &'static str, status: &str, input: u64) -> ClientReceipt {
        let body =
            json!({"model":"gpt-6-luna","input":[{"role":"user","content":"PRIVATE_PROMPT"}]});
        let attempt = Attempt::start(role, "gpt-6-luna", false, true, None, true);
        let mut measurement = dispatch_measurement("parent", role, &body);
        measurement.outgoing_input_tokens = Some(100);
        measurement.basis = "local_visible_v1".into();
        let mut receipt = ClientReceipt::dispatch(
            &attempt,
            &body,
            "http://user:SECRET@localhost/responses?key=SECRET",
            Some(&measurement),
        );
        receipt.observe(Some(&json!({"status":status,"service_tier":"default",
            "usage":{"input_tokens":input,"output_tokens":10,
                "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
                "output_tokens_details":{"reasoning_tokens":5}},
            "error":{"code":"fixture_failure","message":"SECRET_ERROR","param":"invalid arbitrary prose"}})),
            "eof", Some(200), false, 10, true);
        receipt
    }
    #[test]
    fn receipt_owner_freezes_before_usage_and_preserves_all_terminal_money() {
        for role in ["primary", "shadow"] {
            for status in ["completed", "failed", "incomplete", "cancelled"] {
                let receipt = receipt(role, status, 150);
                assert_eq!(receipt.forecast.input_tokens, Some(100));
                assert_eq!(receipt.residual, Some(Residual::between(150, 100)));
                assert_eq!(receipt.provider_status.as_deref(), Some(status));
                assert!(receipt.cost.is_some());
                let mut ledger = carry::core::UsageLedger::default();
                assert_eq!(receipt.settle(&mut ledger), receipt.cost);
                assert_eq!(ledger.calls, 1);
                assert_eq!(ledger.input_tokens, 150);
                let bytes = serde_json::to_vec(&receipt).unwrap();
                let stored: ClientReceipt = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(stored.cost, receipt.cost);
                assert_eq!(stored.forecast.input_tokens, Some(100));
                assert_eq!(stored.usage["output_tokens_details"]["reasoning_tokens"], 5);
                let text = String::from_utf8(bytes).unwrap();
                for secret in ["PRIVATE_PROMPT", "SECRET", "localhost", "arbitrary prose"] {
                    assert!(!text.contains(secret));
                }
                let shape = ClientReceipt::shape(&json!({"model":"gpt-6-luna"}), None);
                assert!(settlement::size_bound(&shape).unwrap() >= text.len() as u64);
            }
        }
    }
    #[test]
    fn calibration_is_paired_and_restorable_not_fitted_to_current_receipt() {
        let mut moments = ErrorMoments::default();
        moments.observe(&receipt("primary", "completed", 150));
        moments.observe(&receipt("primary", "failed", 80));
        assert_eq!(moments.samples, 2);
        assert_eq!(moments.bias_tokens, (50.0 - 20.0) / 2.0);
        assert_eq!(moments.mean_absolute_error_tokens, (50.0 + 20.0) / 2.0);
        let expected =
            ((50.0 - moments.bias_tokens).powi(2) + (-20.0 - moments.bias_tokens).powi(2)) / 2.0;
        assert_eq!(moments.variance_tokens, expected);
        let mut unknown = receipt("primary", "completed", 150);
        unknown.forecast.input_tokens = None;
        unknown.residual = None;
        moments.observe(&unknown);
        assert_eq!(moments.samples, 2);
        assert_eq!(moments.unavailable, 1);
        let restored: ErrorMoments =
            serde_json::from_value(serde_json::to_value(&moments).unwrap()).unwrap();
        assert_eq!(restored.variance_tokens, moments.variance_tokens);
    }
    #[test]
    fn receipt_envelope_covers_full_u64_residual_and_optional_provenance() {
        let body = json!({"model":"gpt-6-luna"});
        for negative in [false, true] {
            let mut shape = ClientReceipt::shape(&body, None);
            shape.transport_ordinal = u8::MAX;
            shape.http_status = Some(u16::MAX);
            shape.latency_ms = u64::MAX;
            shape.forecast.at = u64::MAX;
            shape.forecast.input_tokens = Some(u64::MAX);
            for (_, value) in shape.usage.as_object_mut().unwrap() {
                if let Some(details) = value.as_object_mut() {
                    for (_, value) in details {
                        *value = json!(u64::MAX);
                    }
                } else {
                    *value = json!(u64::MAX);
                }
            }
            shape.residual = Some(Residual {
                negative,
                absolute_tokens: u64::MAX,
            });
            shape.cost = Some(f64::MAX);
            let bytes = serde_json::to_vec(&shape).unwrap();
            let restored: ClientReceipt = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(restored.residual, shape.residual);
            let bound = settlement::size_bound(&ClientReceipt::shape(&body, None)).unwrap();
            assert!(bytes.len() as u64 <= bound);
            assert_eq!(Residual::between(0, u64::MAX).absolute_tokens, u64::MAX);
            assert!(Residual::between(0, u64::MAX).negative);
            assert!(!Residual::between(u64::MAX, 0).negative);
        }
    }

    #[test]
    fn absent_unknown_zero_and_invalid_numeric_partitions_stay_distinct() {
        let mut record = receipt("shadow", "failed", 0);
        record.usage["output_tokens"] = json!(0);
        let mut ledger = carry::core::UsageLedger::default();
        assert_eq!(record.settle(&mut ledger), Some(0.0));
        record.usage["input_tokens_details"]["cached_tokens"] = json!(-3);
        let mut ledger = carry::core::UsageLedger::default();
        assert_eq!(record.settle(&mut ledger), None);
        assert_eq!(ledger.unavailable_cost_calls, 1);
        record.usage = Value::Null;
        let mut ledger = carry::core::UsageLedger::default();
        assert_eq!(record.settle(&mut ledger), None);
        assert_eq!(ledger.calls, 1);
    }
    #[test]
    fn summary_modes_price_new_suffix_and_never_credit_generated_output() {
        use crate::savings::SummaryStrategy::*;
        let price = |input: u64, cached: u64| {
            Some((input - cached) as f64 + cached as f64 * 0.1 + 1000.0 * 4.0)
        };
        let cold = ReplacementCold
            .price(8000, 10000, Some(9000), price)
            .unwrap();
        let appended = AppendedHistory
            .price(8000, 10000, Some(9000), price)
            .unwrap();
        assert_eq!(cold, price(8000 + 64, 0).unwrap());
        assert_eq!(appended, price(10000 + 64, 9000).unwrap());
        assert_eq!(
            ConservativeLower.price(8000, 10000, Some(9000), price),
            Some(cold.min(appended))
        );
        assert_eq!(AppendedHistory.price(8000, 10000, None, price), None);
        assert_eq!(
            AppendedHistory.price(8000, 10000, Some(0), price),
            price(10000 + 64, 0)
        );
    }
}
