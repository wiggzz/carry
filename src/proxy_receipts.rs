//! Receipt-based accounting, separate from the retained legacy JSON estimand.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Receipts {
    anchor: Option<Anchor>,
    // Legacy decoder input only. New history has a typed quantity contract and
    // cursor in TraditionalHistoryState, never this full-model cache digest.
    #[serde(default)]
    history_anchor: Option<Anchor>,
    #[serde(default)]
    pub(super) initialization_policy_version: Option<u32>,
    // Legacy observation cursor only; never reinterpret its terminal time as a write.
    #[serde(default)]
    traditional_anchor: Option<Anchor>,
    #[serde(default)]
    modeled_cache: crate::proxy_cache::ModeledCache,
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
            history_anchor: None,
            initialization_policy_version: Some(1),
            traditional_anchor: None,
            modeled_cache: crate::proxy_cache::ModeledCache::default(),
            configuration: None,
            scenario_cache_policy_version: Some(carry::models::ScenarioCachePolicy::VERSION),
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
        self.modeled_cache.clear();
    }

    pub(super) fn forget_history(&mut self) {
        self.invalidate();
        self.history_anchor = None;
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
    /// Advance only cache-policy identity after proving the old full contract.
    /// Legacy checkpoints retain opaque configuration hashes, so excluding policy
    /// fields from new hashes alone would discard their existing summary history.
    pub(crate) fn rekey_receipt_policy(
        &mut self,
        body: &Value,
        route: &str,
        prior: &ResolvedModel,
        current: &ResolvedModel,
    ) {
        let Some(receipts) = self.receipts.as_mut() else {
            return;
        };
        let prior_version = prior.scenario_cache_policy.as_ref().map(|p| p.version);
        let current_version = current.scenario_cache_policy.as_ref().map(|p| p.version);
        if receipts.scenario_cache_policy_version != prior_version
            || prior_version == current_version
        {
            return;
        }
        let without_policy = |resolution: &ResolvedModel| {
            let mut resolution = resolution.clone();
            resolution.scenario_cache_policy = None;
            resolution.provenance.remove("scenario_cache");
            resolution
        };
        if digest(&without_policy(prior)) != digest(&without_policy(current)) {
            return;
        }
        let previous = configuration(body, route, Some(prior));
        if receipts.configuration.as_ref() != Some(&previous) {
            return;
        }
        let next = configuration(body, route, Some(current));
        receipts.configuration = Some(next.clone());
        for anchor in [
            &mut receipts.anchor,
            &mut receipts.traditional_anchor,
            &mut receipts.history_anchor,
        ]
        .into_iter()
        .flatten()
        {
            if anchor.configuration == previous {
                anchor.configuration = next.clone();
            }
        }
    }

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
        self.history.diverge();
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
        if self
            .receipts
            .as_ref()
            .unwrap()
            .initialization_policy_version
            != Some(1)
        {
            // The legacy window is an immutable cumulative boundary, not an
            // additive ledger. Begin only future comparisons with the new rule.
            let prior = self.snapshot();
            if prior.coverage.primary_responses > 0 || prior.coverage.reviewer_responses > 0 {
                self.result.legacy_initialization_segment = Some(CachePolicySegment::from(&prior));
            }
            self.reset_receipt_epoch();
            self.receipts
                .as_mut()
                .unwrap()
                .initialization_policy_version = Some(1);
        }
        let policy_version = resolved
            .map_or(Some(carry::models::ScenarioCachePolicy::VERSION), |r| {
                r.scenario_cache_policy.as_ref().map(|p| p.version)
            });
        let previous_version = self
            .receipts
            .as_ref()
            .unwrap()
            .scenario_cache_policy_version;
        let cache_policy_changed = previous_version != policy_version;
        if cache_policy_changed {
            // A version-only frozen contract can be reconstructed, but is accepted
            // only when its complete old digest matches. Actual upgrades with
            // changed provenance use the original saved resolution at dispatch.
            if let Some(current) = resolved {
                let mut prior = current.clone();
                if let Some(version) = previous_version {
                    if let Some(policy) = &mut prior.scenario_cache_policy {
                        policy.version = version;
                    }
                } else {
                    prior.scenario_cache_policy = None;
                    prior.provenance.remove("scenario_cache");
                }
                self.rekey_receipt_policy(body, route, &prior, current);
            }
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
            self.receipts
                .as_mut()
                .unwrap()
                .scenario_cache_policy_version = policy_version;
        }
        self.result.coverage.primary_responses += 1;
        let native = receipt(&response["usage"]);
        let model = body["model"].as_str().unwrap_or("");
        let projection = crate::savings::prompt_projection(body);
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
            Some(carry),
            Some((_, _, local)),
            Some(items),
            Some(retained_output),
            Some(protected),
        ) = (
            native,
            observed,
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
        let legacy_configuration = self.receipts.as_ref().unwrap().configuration.clone();
        self.receipts.as_mut().unwrap().configuration = Some(config.clone());
        let previous = self.receipts.as_ref().unwrap().anchor.clone();

        let continuity =
            self.history
                .compatible(&contract, body, native.input_tokens, at, self.carry_edited);
        let history_previous = continuity
            .is_none()
            .then(|| self.receipts.as_ref().unwrap().history_anchor.clone())
            .flatten();
        let compatible = previous.as_ref().is_some_and(|a| {
            append_compatible(a, body, &config)
                && at >= a.at
                && native.input_tokens >= a.input
                && (items.len() > a.items || native.input_tokens == a.input)
        });

        // The legacy opaque anchor is only a decoder input. Once a structured
        // cursor exists, neither full-model cache/pricing identity nor rekeying
        // participates in history recognition.
        let history_compatible = continuity.unwrap_or_else(|| {
            legacy_configuration.as_ref() == Some(&config)
                && history_previous.as_ref().is_some_and(|a| {
                    a.configuration == config
                        && at >= a.at
                        && (self.carry_edited
                            || (append_compatible(a, body, &config)
                                && native.input_tokens >= a.input
                                && (items.len() > a.items || native.input_tokens == a.input)))
                })
        });
        if (continuity.is_some() || history_previous.is_some()) && !history_compatible {
            // Rewind, edit, configuration switch or negative total-input delta:
            // invalidate historical assumptions, not the current projection.
            self.reset_receipt_epoch();
        }
        self.receipts.as_mut().unwrap().last_appended_input = previous
            .as_ref()
            .filter(|_| compatible)
            .map(|a| native.input_tokens - a.input);
        let mut traditional = self.history.input(native.input_tokens, self.restored);

        let fixed = projection_fixed_tokens(body);
        let kept = self.scenario.keep_recent.max(fixed);
        let mut initialized_now = false;
        if self.scenario.valid() && !history_compatible && traditional > budget {
            let initial_kept = kept.max(protected);
            let initialized = initial_kept.saturating_add(self.scenario.summary);
            if initialized > budget || initialized >= traditional {
                self.result.excluded_unpriced_calls += 1;
                self.reset_epoch();
                return;
            }
            self.history
                .summarize(traditional, initial_kept, self.scenario.summary);
            traditional = initialized;
            self.traditional_edited = true;
            initialized_now = true;
        }

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
        // The full resolution remains audit/measurement identity, not cache
        // identity: price, provenance and capacity changes do not rename writes.
        // Typed history continuity above owns append/projection compatibility.
        let cache_scope = digest(&(configuration(body, route, None), resolved.map(|r| &r.route)));
        let mode = request_cache_mode(body, model, resolved);
        let cache = &mut self.receipts.as_mut().unwrap().modeled_cache;
        if cache_policy_changed || self.traditional_edited {
            cache.clear();
        }
        let cached = if mode == carry::models::ScenarioCacheMode::Implicit {
            let prefix = cache.eligible_at(&cache_scope, at, traditional);
            if minimum.is_some_and(|min| prefix >= min) {
                prefix
            } else {
                0
            }
        } else {
            0
        };
        // Identical execution has one paid contribution. Only a genuinely
        // different history asks the cache/tariff forecast a hypothetical price.
        // Sharing the money does NOT seed divergent cache history with real hits.
        let pair = self.history.price_primary(Some(carry), || {
            scenario_request_input_cost_resolved(
                body,
                model,
                traditional,
                cached,
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
        let summarize =
            self.scenario.valid() && total > self.scenario.context_window - self.scenario.reserve;
        let discarded = total.saturating_sub(kept);
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
        if let (Some((carry, traditional_cost)), Some(summary_cost)) = (pair, summary_cost) {
            self.result.initial_compactions += u64::from(initialized_now);
            self.result.carry_cost += carry;
            self.result.traditional_cost += traditional_cost + summary_cost;
            self.result.summary_cost += summary_cost;
            *self.priced_primary_responses.as_mut().unwrap() += 1;
        } else {
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        }
        self.history.last_traditional_input = traditional;
        let cache = &mut self.receipts.as_mut().unwrap().modeled_cache;
        if mode == carry::models::ScenarioCacheMode::Implicit
            && minimum.is_some_and(|min| traditional >= min)
        {
            cache.written(cache_scope, at, ttl, traditional, cached);
        } else {
            cache.clear();
        }
        self.carry_edited = false;
        self.traditional_edited = false;
        if summarize {
            self.result.traditional_compactions += 1;
            self.history.summarize(total, kept, self.scenario.summary);
            self.traditional_edited = true;
            self.receipts.as_mut().unwrap().modeled_cache.clear();
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
        let receipts = self.receipts.as_mut().unwrap();
        receipts.anchor = Some(anchor);
        receipts.traditional_anchor = None;
        receipts.history_anchor = None;
    }
}

fn projection_fixed_tokens(body: &Value) -> u64 {
    // The receipt boundary has already validated the input array.
    crate::savings::fixed_prompt_tokens(body).unwrap()
}
