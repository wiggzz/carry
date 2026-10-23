//! Receipt-based accounting, separate from the retained legacy JSON estimand.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct Receipts {
    anchor: Option<Anchor>,
    pub last_appended_input: Option<u64>,
    pub legacy_estimate: Option<Box<Estimate>>,
    epochs: u64,
}

/// One numeric prefix, not a body journal or per-message allocation. Fork
/// hydration creates a fresh trajectory; an owner never inherits sibling data.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Anchor {
    configuration: String,
    prefix: String,
    items: usize,
    input: u64,
    local_input: u64,
    at: u64,
}

impl Receipts {
    pub(super) fn invalidate(&mut self) {
        self.anchor = None;
        self.last_appended_input = None;
        self.epochs = self.epochs.saturating_add(1);
    }
}

fn digest<T: Serialize + ?Sized>(value: &T) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

fn configuration(body: &Value, route: &str, resolved: Option<&ResolvedModel>) -> String {
    let mut base = body.clone();
    if let Some(object) = base.as_object_mut() {
        // These do not affect model-visible input. Everything else is fenced,
        // including tools, instructions, output/reasoning settings and tier.
        for key in ["input", "stream", "store", "metadata", "include"] {
            object.remove(key);
        }
    }
    digest(&(base, route, resolved))
}

fn append_compatible(anchor: &Anchor, body: &Value, configuration: &str) -> bool {
    anchor.configuration == configuration
        && body["input"].as_array().is_some_and(|items| {
            items.len() >= anchor.items && digest(&items[..anchor.items]) == anchor.prefix
        })
}

/// Explicit numeric partitions only. Absent optional write counts mean no
/// reported write (Responses semantics); absent cache-read data is unknown.
/// Reject contradictory aliases rather than clipping them into a priced call.
fn receipt(usage: &Value) -> Option<Usage> {
    fn partition(values: &[&Value], required: bool) -> Option<u64> {
        let mut found = None;
        for value in values.iter().filter(|v| !v.is_null()) {
            let n = value.as_u64()?;
            if found.is_some_and(|prior| prior != n) {
                return None;
            }
            found = Some(n);
        }
        if required {
            found
        } else {
            Some(found.unwrap_or(0))
        }
    }
    let input = usage["input_tokens"].as_u64()?;
    let output = usage["output_tokens"].as_u64()?;
    let cached = partition(
        &[
            &usage["input_tokens_details"]["cached_tokens"],
            &usage["cached_input_tokens"],
        ],
        true,
    )?;
    let written = partition(
        &[
            &usage["input_tokens_details"]["cache_write_tokens"],
            &usage["input_tokens_details"]["cache_creation_tokens"],
            &usage["cache_write_input_tokens"],
            &usage["cache_creation_input_tokens"],
        ],
        false,
    )?;
    if cached.checked_add(written)? > input {
        return None;
    }
    Some(Usage {
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: cached,
        cache_write_input_tokens: written,
        ..Usage::default()
    })
}

impl Trajectory {
    /// Freeze old sums on the first post-upgrade request, never reprice them.
    /// The new window starts from the retained input, not an invented empty chat.
    pub fn begin_receipt_window(&mut self) {
        if self.receipts.is_some() {
            return;
        }
        let legacy = self.snapshot();
        let scenario = self.scenario;
        *self = Self {
            scenario,
            ..Self::default()
        };
        self.receipts.as_mut().unwrap().legacy_estimate = Some(Box::new(legacy));
    }

    pub fn receipt_prediction(
        &self,
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<u64> {
        let anchor = self.receipts.as_ref()?.anchor.as_ref()?;
        if !append_compatible(anchor, body, &configuration(body, route, resolved)) {
            return None;
        }
        let (_, _, local) = crate::savings::prompt_projection(body)?;
        // Only the appended block is estimated. Never scale individual messages
        // by a global calibration ratio or add billed/hidden output to input.
        Some(
            anchor
                .input
                .saturating_add(local.saturating_sub(anchor.local_input)),
        )
    }

    pub(super) fn observed_reviewer(
        &mut self,
        model: &str,
        usage: &Value,
        standard: bool,
        resolved: Option<&ResolvedModel>,
    ) {
        self.result.coverage.reviewer_responses += 1;
        let native = receipt(usage);
        let cost = native
            .as_ref()
            .and_then(|u| modeled_cost(model, u, resolved));
        if let Some(cost) = cost {
            self.result.observed_reviewer_cost += cost;
            self.result.reviewer_cost += cost;
            self.result.carry_cost += cost;
        } else {
            self.result.unavailable_reviewer_cost_calls += 1;
            self.result.excluded_unpriced_calls += 1;
            self.result.excluded_unpriced_reviewer_calls += 1;
            if native.is_none() {
                self.result.coverage.missing_usage += 1;
            } else {
                self.result.coverage.unknown_rate_calls += 1;
            }
        }
        if !standard {
            self.result.coverage.unmodeled_calls += 1;
        }
    }

    pub(super) fn observed_primary(
        &mut self,
        body: &Value,
        response: &Value,
        route: &str,
        at: u64,
        standard: bool,
        resolved: Option<&ResolvedModel>,
    ) {
        if response["status"] != "completed" {
            self.failed_call();
            return;
        }
        self.result.coverage.primary_responses += 1;
        let native = receipt(&response["usage"]);
        let model = body["model"].as_str().unwrap_or("");
        let observed = native
            .as_ref()
            .and_then(|u| modeled_cost(model, u, resolved));
        if let Some(cost) = observed {
            self.result.observed_primary_cost += cost;
        } else {
            self.result.unavailable_primary_cost_calls += 1;
            if native.is_none() {
                self.result.coverage.missing_usage += 1;
            } else {
                self.result.coverage.unknown_rate_calls += 1;
            }
        }
        let projection = crate::savings::prompt_projection(body);
        let text = standard
            && !has_native_modalities(body)
            && !has_native_modalities(&response["output"])
            && !["input_tokens_details", "output_tokens_details"]
                .iter()
                .any(|f| {
                    response["usage"][*f]["audio_tokens"]
                        .as_u64()
                        .is_some_and(|n| n > 0)
                });
        let (Some(native), Some(carry), Some((_, _, local)), Some(items), Some(output)) = (
            native,
            observed,
            projection,
            body["input"].as_array(),
            response["output"].as_array(),
        ) else {
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        };
        if !text {
            self.result.coverage.unmodeled_calls += 1;
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        }
        let config = configuration(body, route, resolved);
        let previous = self.receipts.as_ref().unwrap().anchor.clone();
        let compatible = previous
            .as_ref()
            .is_some_and(|a| append_compatible(a, body, &config) && native.input_tokens >= a.input);
        if previous.is_some() && !compatible && !self.carry_edited {
            // Rewind, edit, configuration switch or negative total-input delta:
            // start a new comparison epoch, not a guessed removal allocation.
            self.reset_epoch();
        }
        self.receipts.as_mut().unwrap().last_appended_input = previous
            .as_ref()
            .filter(|_| compatible)
            .map(|a| native.input_tokens - a.input);
        let traditional = (native.input_tokens as i128)
            .saturating_add(self.restored)
            .saturating_sub(self.retired as i128)
            .saturating_add(self.summary as i128)
            .clamp(0, u64::MAX as i128) as u64;
        let unchanged = self.restored == 0 && self.retired == 0 && self.summary == 0;
        let minimum = resolved
            .map_or_else(|| prompt_cache_capabilities(model), |r| r.cache)
            .map(|c| c.minimum_prefix_tokens as u64);
        let warm = previous
            .as_ref()
            .is_some_and(|a| compatible && at >= a.at && at - a.at < 1800)
            && !self.traditional_edited;
        // Identical branches share the receipt. After a modeled edit/summary,
        // the alternative uses an explicitly conditional append-cache scenario.
        let traditional_cost = if unchanged {
            Some(carry)
        } else {
            scenario_input_cost_resolved(
                model,
                traditional,
                if warm {
                    self.last_traditional_input.min(traditional)
                } else {
                    0
                },
                minimum,
                resolved,
            )
            .and_then(|input| {
                Some(
                    input
                        + scenario_output_cost_resolved(
                            model,
                            traditional,
                            native.output_tokens,
                            resolved,
                        )?,
                )
            })
        };
        // Retained output is locally estimated, not billed reasoning/output.
        let total = traditional.saturating_add(token_units(&Value::Array(output.clone())));
        let summarize =
            self.scenario.valid() && total > self.scenario.context_window - self.scenario.reserve;
        let discarded = total.saturating_sub(self.scenario.keep_recent);
        let summary_cost = if summarize {
            modeled_cost(
                model,
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
        if let (Some(traditional_cost), Some(summary_cost)) = (traditional_cost, summary_cost) {
            self.result.carry_cost += carry;
            self.result.traditional_cost += traditional_cost + summary_cost;
            self.result.summary_cost += summary_cost;
            *self.priced_primary_responses.as_mut().unwrap() += 1;
        } else {
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        }
        self.last_traditional_input = traditional;
        self.carry_edited = false;
        self.traditional_edited = false;
        if summarize {
            self.result.traditional_compactions += 1;
            self.retired = self
                .retired
                .saturating_add(discarded.saturating_sub(self.summary));
            self.summary = self.scenario.summary;
            self.last_traditional_input = self.scenario.keep_recent.saturating_add(self.summary);
            self.traditional_edited = true;
        }
        self.receipts.as_mut().unwrap().anchor = Some(Anchor {
            configuration: config,
            prefix: digest(items),
            items: items.len(),
            input: native.input_tokens,
            local_input: local,
            at,
        });
    }
}
