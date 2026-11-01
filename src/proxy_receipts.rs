//! Receipt-based proxy accounting with dispatch-frozen observed money.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Receipts {
    anchor: Option<Anchor>,
    #[serde(default)]
    pub(super) initialization_policy_version: Option<u32>,
    #[serde(default)]
    modeled_cache: crate::proxy_cache::ModeledCache,
    #[serde(default)]
    pub scenario_cache_policy_version: Option<u32>,
    #[serde(default)]
    configuration: Option<String>,
    pub last_appended_input: Option<u64>,
    epochs: u64,
}
impl Default for Receipts {
    fn default() -> Self {
        Self {
            anchor: None,
            initialization_policy_version: Some(1),
            modeled_cache: crate::proxy_cache::ModeledCache::default(),
            configuration: None,
            scenario_cache_policy_version: Some(carry::models::ScenarioCachePolicy::VERSION),
            last_appended_input: None,
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
    // Observation ordering for native append prediction, not cache write time.
    at: u64,
}

impl Receipts {
    pub(super) fn invalidate(&mut self) {
        self.invalidate_actual();
        self.modeled_cache.clear();
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
pub(super) fn receipt(usage: &Value) -> Option<Usage> {
    Usage::from_receipt(usage)
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

/// Responses image inputs are billed in the request's native input tokens.
/// Audio/video and generated image output can have independent tariffs, so they
/// remain unsupported by this token-only tariff contract.
fn has_unpriced_modalities(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    kind != "input_image"
                        && ["image", "audio", "video"].iter().any(|m| kind.contains(m))
                })
                || object.values().any(has_unpriced_modalities)
        }
        Value::Array(items) => items.iter().any(has_unpriced_modalities),
        _ => false,
    }
}

impl Trajectory {
    pub fn receipt_prediction(
        &self,
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<u64> {
        let anchor = self.receipts.anchor.as_ref()?;
        if !append_compatible(anchor, body, &configuration(body, route, resolved)) {
            return None;
        }
        let items = body["input"].as_array()?;
        if items.len() == anchor.items {
            return Some(anchor.input);
        }
        if has_native_modalities(&json!(&items[anchor.items..])) {
            // A receipt measured the old prefix, not a newly appended image.
            // Never tokenize its URL/base64 and present that as image units.
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
        usage: &Value,
        standard: bool,
        settled: Option<f64>,
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
        let cost = native.as_ref().filter(|_| text).and(settled);
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

    pub(super) fn observed_primary(&mut self, primary: PrimaryObservation<'_>) {
        let PrimaryObservation {
            body,
            response,
            route,
            at,
            standard,
            resolved,
            cost: settled,
        } = primary;
        if response["status"] != "completed" {
            self.failed_call();
            return;
        }
        if self.receipts.initialization_policy_version != Some(1) {
            // The legacy window is an immutable cumulative boundary, not an
            // additive ledger. Begin only future comparisons with the new rule.
            let prior = self.snapshot();
            if prior.coverage.primary_responses > 0 || prior.coverage.reviewer_responses > 0 {
                self.result.legacy_initialization_segment = Some(CachePolicySegment::from(&prior));
            }
            self.reset_receipt_epoch();
            self.receipts.initialization_policy_version = Some(1);
        }
        let policy_version = resolved
            .map_or(Some(carry::models::ScenarioCachePolicy::VERSION), |r| {
                r.scenario_cache_policy.as_ref().map(|p| p.version)
            });
        let previous_version = self.receipts.scenario_cache_policy_version;
        let cache_policy_changed = previous_version != policy_version;
        if cache_policy_changed {
            // Freeze cumulative provenance before ANY version transition. Cache
            // becomes cold, but ancestry checks still own summary invalidation.
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
            self.receipts.scenario_cache_policy_version = policy_version;
        }
        let model = body["model"].as_str().unwrap_or("");
        let projection = crate::savings::prompt_projection(body);
        let (native, observed, billable) =
            self.record_primary_observation(body, response, standard, settled);
        let retained_output = retained_output_tokens(&response["output"]);
        let allowance = carry::models::output_allowance(
            body["max_output_tokens"].as_u64(),
            resolved.map_or(self.scenario.reserve, |r| {
                u64::from(r.ordinary.output_reserve)
            }),
        );
        let budget = self.scenario.initial_input_budget(
            allowance,
            resolved.and_then(|r| r.capacity_tokens.map(u64::from)),
        );
        // An unchanged image-bearing request can share its exact receipt total
        // between both branches. A projected/restored/summary slice cannot use
        // base64 length or an invented per-image allocation as native tokens.
        let image_input = has_native_modalities(body);
        let image_comparable = !image_input
            || (!self.carry_edited
                && self.restored == 0
                && self.history.retired == 0
                && self.history.summary == 0
                && native
                    .as_ref()
                    .zip(retained_output)
                    .is_some_and(|(u, retained)| {
                        !self.scenario.valid()
                            || (u.input_tokens <= budget
                                && u.input_tokens.saturating_add(retained)
                                    <= self.scenario.context_window - self.scenario.reserve)
                    }));
        if !billable || !image_comparable || retained_output.is_none() {
            self.result.coverage.unmodeled_calls += 1;
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        }
        let (
            Some(native),
            Some((_, _, local)),
            Some(items),
            Some(retained_output),
            Some(protected),
        ) = (
            native,
            projection,
            body["input"].as_array(),
            retained_output,
            crate::savings::protected_prompt_tokens(body),
        )
        else {
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        };
        let config = configuration(body, route, resolved);
        let contract = HistoryContract::extract(body, route, resolved, self.scenario);
        self.receipts.configuration = Some(config.clone());
        let previous = self.receipts.anchor.clone();

        let continuity =
            self.history
                .compatible(&contract, body, native.input_tokens, at, self.carry_edited);

        let compatible = previous.as_ref().is_some_and(|a| {
            append_compatible(a, body, &config)
                && at >= a.at
                && native.input_tokens >= a.input
                && (items.len() > a.items || native.input_tokens == a.input)
        });

        let history_compatible = continuity.unwrap_or(false);
        if continuity.is_some() && !history_compatible {
            // Rewind, edit, configuration switch or negative total-input delta:
            // invalidate historical assumptions, not the current projection.
            self.reset_receipt_epoch();
        }
        self.receipts.last_appended_input = previous
            .as_ref()
            .filter(|_| compatible)
            .map(|a| native.input_tokens - a.input);
        let mut traditional = self.history.input(native.input_tokens, self.restored);

        let fixed = projection_fixed_tokens(body);
        let initialized_now = match self.scenario.initial_compaction(
            history_compatible,
            traditional,
            protected,
            allowance,
            resolved.and_then(|r| r.capacity_tokens.map(u64::from)),
        ) {
            Ok(Some(kept)) => {
                self.history
                    .summarize(traditional, kept, self.scenario.summary);
                traditional = self.history.last_traditional_input;
                self.traditional_edited = true;
                true
            }
            Ok(None) => false,
            Err(()) => {
                self.result.excluded_unpriced_calls += 1;
                self.reset_epoch();
                return;
            }
        };

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
        if minimum.is_none()
            || request_cache_mode(body, model, resolved)
                == carry::models::ScenarioCacheMode::Unknown
        {
            self.result.coverage.unknown_cache_requests += 1;
        }
        let ttl = resolved
            .and_then(|r| r.scenario_cache_policy.as_ref())
            .map_or(1800, |p| p.ttl_seconds);
        // Typed history continuity owns append/projection compatibility. The
        // shared cache owner, not raw request settings, owns its namespace.
        let cache_scope =
            crate::proxy_cache::modeled_scope(body, model, (route, resolved.map(|r| &r.route)));
        let mode = request_cache_mode(body, model, resolved);
        let cache = &mut self.receipts.modeled_cache;
        if cache_policy_changed || self.traditional_edited {
            cache.clear();
        }
        let cached = if mode == carry::models::ScenarioCacheMode::Implicit {
            cache
                .eligible_at(&cache_scope, at, traditional)
                .map(|prefix| {
                    if minimum.is_some_and(|min| prefix >= min) {
                        prefix
                    } else {
                        0
                    }
                })
        } else {
            Some(0)
        };
        // Identical execution has one paid contribution. Only a genuinely
        // different history asks the cache/tariff forecast a hypothetical price.
        // Sharing the money does NOT seed divergent cache history with real hits.
        let pair = self.history.price_primary(observed, || {
            scenario_request_input_cost_resolved(
                body,
                model,
                traditional,
                cached?,
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
        });
        // Only the explicit semantic projection can enter this estimate;
        // opaque transport size and billed output are not retained input units.
        let total = traditional.saturating_add(retained_output);
        let kept = match self
            .scenario
            .response_compaction(traditional, retained_output, fixed)
        {
            Ok(kept) => kept,
            Err(()) => {
                self.result.excluded_unpriced_calls += 1;
                self.reset_epoch();
                return;
            }
        };
        let summary_cost = if let Some(kept) = kept {
            modeled_cost(
                model,
                &Usage {
                    input_tokens: total.saturating_sub(kept),
                    output_tokens: self.scenario.summary,
                    ..Usage::default()
                },
                resolved,
            )
        } else {
            Some(0.0)
        };
        // Quantity/projection transitions belong to history, not monetary-pair
        // admission. An unavailable tariff excludes money without losing the
        // successful native cursor or granting another free initialization.
        self.result.initial_compactions += u64::from(initialized_now);
        if let (Some((carry, traditional_cost)), Some(summary_cost)) = (pair, summary_cost) {
            self.result.carry_cost += carry;
            self.result.traditional_cost += traditional_cost + summary_cost;
            self.result.summary_cost += summary_cost;
            self.priced_primary_responses += 1;
        } else {
            self.result.excluded_unpriced_calls += 1;
            if cached.is_none() {
                self.result.coverage.unknown_cache_requests += 1;
            }
        }
        self.history.last_traditional_input = traditional;
        let cache = &mut self.receipts.modeled_cache;
        if mode == carry::models::ScenarioCacheMode::Implicit
            && minimum.is_some_and(|min| traditional >= min)
        {
            let read_refresh = resolved
                .and_then(|r| r.scenario_cache_policy.as_ref())
                .and_then(|policy| policy.read_refresh_seconds(model, body));
            cache.requested(cache_scope, at, ttl, traditional, read_refresh);
        } else {
            cache.clear();
        }
        self.carry_edited = false;
        self.traditional_edited = false;
        if let Some(kept) = kept {
            self.result.traditional_compactions += 1;
            self.history.summarize(total, kept, self.scenario.summary);
            self.traditional_edited = true;
            self.receipts.modeled_cache.clear();
        }
        self.history
            .observe(contract, items, native.input_tokens, at);
        let anchor = Anchor {
            configuration: config,
            prefix: digest(items),
            items: items.len(),
            input: native.input_tokens,
            local_input: local,
            at,
        };
        let receipts = &mut self.receipts;
        receipts.anchor = Some(anchor);
    }

    fn record_primary_observation(
        &mut self,
        body: &Value,
        response: &Value,
        standard: bool,
        settled: Option<f64>,
    ) -> (Option<Usage>, Option<f64>, bool) {
        self.result.coverage.primary_responses += 1;
        let native = receipt(&response["usage"]);
        let billable = standard
            && !has_unpriced_modalities(body)
            && !has_native_modalities(&response["output"])
            && !["input_tokens_details", "output_tokens_details"]
                .iter()
                .any(|f| {
                    response["usage"][*f]["audio_tokens"]
                        .as_u64()
                        .is_some_and(|n| n > 0)
                });
        let observed = native.as_ref().filter(|_| billable).and(settled);
        if let Some(cost) = observed {
            self.result.observed_primary_cost += cost;
        } else {
            self.result.unavailable_primary_cost_calls += 1;
            if native.is_none() {
                self.result.coverage.missing_usage += 1;
            } else if billable {
                self.result.coverage.unknown_rate_calls += 1;
            }
        }
        (native, observed, billable)
    }
}

fn projection_fixed_tokens(body: &Value) -> u64 {
    // The receipt boundary has already validated the input array.
    crate::savings::fixed_prompt_tokens(body).unwrap()
}
