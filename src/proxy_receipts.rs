//! Receipt-based accounting, separate from the retained legacy JSON estimand.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Receipts {
    anchor: Option<Anchor>,
    #[serde(default)]
    traditional_anchor: Option<Anchor>,
    #[serde(default)]
    pub scenario_cache_policy_version: Option<u32>,
    #[serde(default)]
    configuration: Option<String>,
    pub last_appended_input: Option<u64>,
    pub legacy_estimate: Option<Box<Estimate>>,
    epochs: u64,
}
impl Default for Receipts {
    fn default() -> Self {
        Self {
            anchor: None,
            traditional_anchor: None,
            configuration: None,
            scenario_cache_policy_version: Some(1),
            last_appended_input: None,
            legacy_estimate: None,
            epochs: 0,
        }
    }
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
        self.invalidate_actual();
        self.traditional_anchor = None;
    }

    pub(super) fn invalidate_actual(&mut self) {
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
    fn partition(values: &[Option<&Value>], required: bool) -> Option<u64> {
        let mut found = None;
        for value in values.iter().flatten() {
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
            usage["input_tokens_details"].get("cached_tokens"),
            usage.get("cached_input_tokens"),
        ],
        true,
    )?;
    let written = partition(
        &[
            usage["input_tokens_details"].get("cache_write_tokens"),
            usage["input_tokens_details"].get("cache_creation_tokens"),
            usage.get("cache_write_input_tokens"),
            usage.get("cache_creation_input_tokens"),
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

/// Estimate only semantic text that can be retained in the next input. Native
/// output billing, IDs and encrypted transport envelopes are different units.
/// Unknown output kinds have no invented retained-context measurement.
fn retained_output_tokens(output: &Value) -> Option<u64> {
    let mut text = Vec::new();
    for item in output.as_array()? {
        match item["type"].as_str()? {
            "message" => {
                if let Some(content) = item["content"].as_str() {
                    text.push(content);
                } else {
                    for part in item["content"].as_array()? {
                        text.push(match part["type"].as_str()? {
                            "output_text" | "input_text" => part["text"].as_str()?,
                            "refusal" => part["refusal"].as_str()?,
                            _ => return None,
                        });
                    }
                }
            }
            "reasoning" => {
                if let Some(summary) = item.get("summary") {
                    for part in summary.as_array()? {
                        text.push(part["text"].as_str()?);
                    }
                }
            }
            "function_call" => {
                text.push(item["name"].as_str()?);
                text.push(item["arguments"].as_str()?);
            }
            "custom_tool_call" => {
                text.push(item["name"].as_str()?);
                text.push(item["input"].as_str()?);
            }
            _ => return None,
        }
    }
    Some(if text.is_empty() {
        0
    } else {
        token_units(&json!(text))
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
        let text = standard
            && !["input_tokens_details", "output_tokens_details"]
                .iter()
                .any(|field| {
                    usage[*field]["audio_tokens"]
                        .as_u64()
                        .is_some_and(|n| n > 0)
                });
        let cost = native
            .as_ref()
            .filter(|_| text)
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
            } else if text {
                self.result.coverage.unknown_rate_calls += 1;
            }
        }
        if !text {
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
        let policy_version = resolved.map_or(Some(1), |r| {
            r.scenario_cache_policy.as_ref().map(|p| p.version)
        });
        let previous_version = self
            .receipts
            .as_ref()
            .unwrap()
            .scenario_cache_policy_version;
        if previous_version != policy_version {
            // Freeze cumulative provenance before ANY version transition, not
            // just missing -> present. Never replay, reset money or overwrite
            // earlier boundaries. The next comparison starts cold.
            let prior = self.snapshot();
            if prior.coverage.primary_responses > 0 || prior.coverage.reviewer_responses > 0 {
                let cumulative = CachePolicySegment::from(&prior);
                if previous_version.is_none()
                    && policy_version.is_some()
                    && self.result.legacy_cache_policy_segment.is_none()
                    && self.result.cache_policy_boundaries.is_empty()
                {
                    self.result.legacy_cache_policy_segment = Some(cumulative.clone());
                }
                self.result
                    .cache_policy_boundaries
                    .push(CachePolicyBoundary {
                        previous_version,
                        next_version: policy_version,
                        cumulative,
                    });
            }
            self.reset_receipt_epoch();
            self.receipts
                .as_mut()
                .unwrap()
                .scenario_cache_policy_version = policy_version;
        }
        self.result.coverage.primary_responses += 1;
        let native = receipt(&response["usage"]);
        let model = body["model"].as_str().unwrap_or("");
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
        let observed = native
            .as_ref()
            .filter(|_| text)
            .and_then(|u| modeled_cost(model, u, resolved));
        if let Some(cost) = observed {
            self.result.observed_primary_cost += cost;
        } else {
            self.result.unavailable_primary_cost_calls += 1;
            if native.is_none() {
                self.result.coverage.missing_usage += 1;
            } else if text {
                self.result.coverage.unknown_rate_calls += 1;
            }
        }
        if !text {
            self.result.coverage.unmodeled_calls += 1;
        }
        let retained_output = retained_output_tokens(&response["output"]);
        if text && retained_output.is_none() {
            self.result.coverage.unmodeled_calls += 1;
        }
        let (Some(native), Some(carry), Some((_, _, local)), Some(items), Some(retained_output)) = (
            native,
            observed,
            projection,
            body["input"].as_array(),
            retained_output,
        ) else {
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        };
        let config = configuration(body, route, resolved);
        if self
            .receipts
            .as_ref()
            .unwrap()
            .configuration
            .as_ref()
            .is_some_and(|prior| prior != &config)
        {
            self.reset_receipt_epoch();
        }
        self.receipts.as_mut().unwrap().configuration = Some(config.clone());
        let previous = self.receipts.as_ref().unwrap().anchor.clone();
        let traditional_previous = self.receipts.as_ref().unwrap().traditional_anchor.clone();
        let compatible = previous.as_ref().is_some_and(|a| {
            append_compatible(a, body, &config)
                && at >= a.at
                && native.input_tokens >= a.input
                && (items.len() > a.items || native.input_tokens == a.input)
        });
        let traditional_compatible = traditional_previous.as_ref().is_some_and(|a| {
            a.configuration == config && at >= a.at && (self.carry_edited || compatible)
        });
        if (previous.is_some() && !compatible && !self.carry_edited)
            || (traditional_previous.is_some() && !traditional_compatible)
        {
            // Rewind, edit, configuration switch or negative total-input delta:
            // invalidate historical assumptions, not the current projection.
            self.reset_receipt_epoch();
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

        // The alternative models an API-equivalent policy, not this route's
        // ability to request an explicit breakpoint or promise a future hit.
        // Old resolved snapshots remain explicitly legacy until resolution
        // upgrades their prospective policy; never consult a live catalog here.
        let minimum = resolved.map_or_else(
            || carry::models::ScenarioCachePolicy::for_model(model).minimum_prefix_tokens,
            |r| {
                r.scenario_cache_policy.as_ref().map_or_else(
                    || r.cache.map(|c| c.minimum_prefix_tokens as u64),
                    |p| p.minimum_prefix_tokens,
                )
            },
        );
        if minimum.is_none() {
            self.result.coverage.unknown_cache_requests += 1;
        }
        let ttl = resolved
            .and_then(|r| r.scenario_cache_policy.as_ref())
            .map_or(1800, |p| p.ttl_seconds);
        let warm = traditional_previous
            .as_ref()
            .is_some_and(|a| traditional_compatible && at - a.at < ttl)
            && !self.traditional_edited;
        // The alternative shares total-input units, never observed cache hits:
        // cache behavior is a conditional append-prefix scenario even before
        // the first hypothetical edit. The first request of an epoch is cold.
        let traditional_cost = {
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
        // Only the explicit semantic projection can enter this estimate;
        // opaque transport size and billed output are not retained input units.
        let total = traditional.saturating_add(retained_output);
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
        let anchor = Anchor {
            configuration: config,
            prefix: digest(items),
            items: items.len(),
            input: native.input_tokens,
            local_input: local,
            at,
        };
        let receipts = self.receipts.as_mut().unwrap();
        receipts.anchor = Some(anchor.clone());
        receipts.traditional_anchor = Some(anchor);
    }
}
