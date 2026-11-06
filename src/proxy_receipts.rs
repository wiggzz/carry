//! Receipt-based proxy accounting with dispatch-frozen observed money.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Receipts {
    anchor: Option<Anchor>,
    #[serde(default)]
    pub(super) quantity_policy_version: Option<u32>,
    #[serde(default)]
    pub(super) initialization_policy_version: Option<u32>,
    #[serde(default)]
    modeled_cache: crate::proxy_cache::ModeledCache,
    // Initialization belongs to the observed comparison window, not to every
    // later namespace change or known edit, which really can invalidate cache.
    #[serde(default)]
    cache_initialized: bool,
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
            quantity_policy_version: Some(1),
            initialization_policy_version: Some(1),
            modeled_cache: crate::proxy_cache::ModeledCache::default(),
            cache_initialized: false,
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
pub(super) fn retained_output_tokens(output: &Value) -> Option<u64> {
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
            measurements,
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
        if self.receipts.quantity_policy_version != Some(1) {
            let prior = self.snapshot();
            if prior.coverage.primary_responses > 0 || prior.coverage.reviewer_responses > 0 {
                self.result.legacy_quantity_segment = Some(CachePolicySegment::from(&prior));
            }
            // Prospective only: retain past sums, exclusions and the existing
            // history/cache owner. Never replay/reprice old opaque omissions.
            self.receipts.quantity_policy_version = Some(1);
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
        let (native, observed, billable) =
            self.record_primary_observation(body, response, standard, settled);
        let retained_extra = if crate::savings::prompt_has_opaque(&response["output"]) {
            measurements.and_then(|m| m.retained_reasoning_extra(&response["output"]))
        } else {
            Some((0, 0))
        };
        let retained_output = retained_output_tokens(&response["output"])
            .zip(retained_extra)
            .and_then(|(visible, (extra, _))| visible.checked_add(extra));
        let estimated_quantity = self.estimated_projection
            || self.history.summary > 0
            || retained_output.is_some_and(|n| n > 0)
            || retained_extra.is_some_and(|(_, n)| n > 0)
            || self.result.coverage.unavailable_quantity_calls > 0;
        if !self.projection_known || retained_output.is_none() || native.is_none() {
            self.result.coverage.unavailable_quantity_calls += 1;
        }
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
        // Only reasoning has the neutral residual model. Preserve the existing
        // unavailable native slice contract for images/unsupported modalities.
        let unsupported_opaque = crate::savings::prompt_has_opaque(body)
            && !crate::proxy_quantity::accounting_retirement_supported(body);
        let opaque_comparable = !unsupported_opaque
            || (self.projection_known
                && self.history.retired == 0
                && self.history.summary == 0
                && native
                    .as_ref()
                    .zip(retained_output)
                    .is_some_and(|(usage, retained)| {
                        let traditional = (usage.input_tokens as i128 + self.restored)
                            .clamp(0, u64::MAX as i128)
                            as u64;
                        !self.scenario.valid()
                            || (traditional <= budget
                                && traditional.saturating_add(retained)
                                    <= self
                                        .scenario
                                        .context_window
                                        .saturating_sub(self.scenario.reserve))
                    }));
        // The provider measured the WHOLE input, opaque members included.
        // Retirement is the existing scenario's estimated numeric policy, not
        // a claim that a particular encrypted retained slice was measured.
        // Known restoration remains required; native expense is independent.
        if !opaque_comparable
            && self.projection_known
            && retained_output.is_some()
            && native.is_some()
        {
            self.result.coverage.unavailable_quantity_calls += 1;
        }
        if billable && !self.projection_known {
            self.result.coverage.unmodeled_calls += 1;
            self.result.excluded_unpriced_calls += 1;
            // A successful quantity gap is not ancestry evidence. Preserve paid
            // summaries, the established window and its last trustworthy cursor.
            // This request's unknown canonical input/output transition cannot be
            // priced merely because a later projection happens to be measurable.
            self.history.defer_projection(
                configuration(body, route, resolved),
                body,
                response,
                native.as_ref().map(|n| n.input_tokens),
                at,
            );
            self.history.diverge();
            self.receipts.cache_initialized = true;
            if self.history.history_continuity.is_none()
                && let (Some(native), Some(items)) = (native.as_ref(), body["input"].as_array())
            {
                self.history.observe(
                    HistoryContract::extract(body, route, resolved, self.scenario),
                    items,
                    native.input_tokens,
                    at,
                );
            }
            return;
        }
        if !billable || !self.projection_known || !opaque_comparable {
            self.result.coverage.unmodeled_calls += 1;
            self.result.excluded_unpriced_calls += 1;
            self.reset_epoch();
            return;
        }
        let (Some(native), Some(items)) = (native, body["input"].as_array()) else {
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
        let mut recovered_cache_request_at = None;
        if self.history.unresolved_projection
            && self.history.pending_retention.is_none()
            && self.restored == 0
            && !cache_policy_changed
            && let Some(prior_at) =
                self.history
                    .recover_projection(&config, body, native.input_tokens, at)
        {
            // A single empty-output gap can be resolved by an exact repeat
            // now known to be canonical (zero restoration). Recover only
            // its transitions, never its excluded monetary contribution.
            let prior_input = self.history.input(native.input_tokens, 0);
            match self
                .scenario
                .response_compaction(prior_input, 0, projection_fixed_tokens(body))
            {
                Ok(Some(kept)) => {
                    self.history
                        .summarize(prior_input, kept, self.scenario.summary);
                    self.result.traditional_compactions += 1;
                    self.traditional_edited = true;
                    self.receipts.modeled_cache.clear();
                }
                Ok(None) => recovered_cache_request_at = Some(prior_at),
                Err(()) => self.history.unresolved_projection = true,
            }
        }
        if self.history.unresolved_projection {
            self.history.invalidate_projection_recovery();
            self.result.coverage.unavailable_quantity_calls += u64::from(retained_output.is_some());
            self.result.coverage.unmodeled_calls += 1;
            self.result.excluded_unpriced_calls += 1;
            return;
        }
        if self.history.pending_retention.is_some() {
            let recovered = self.history.resolve_retention(
                primary,
                native.input_tokens,
                self.restored,
                self.carry_edited,
                self.scenario,
            );
            let resolved_transition = recovered.and_then(|(retained, fixed)| {
                self.scenario
                    .response_compaction(self.history.last_traditional_input, retained, fixed)
                    .ok()
                    .map(|kept| (retained, kept))
            });
            let Some((retained, kept)) = resolved_transition else {
                // This successful request itself introduces another transition.
                // Retaining only the older pending response cannot later replay
                // this request's summaries/cache/output honestly. Keep the gap
                // explicitly unavailable rather than resume from a stale cursor.
                self.history.unresolved_projection = true;
                self.result.coverage.unavailable_quantity_calls +=
                    u64::from(retained_output.is_some());
                self.result.coverage.unmodeled_calls += 1;
                self.result.excluded_unpriced_calls += 1;
                return;
            };
            if let Some(kept) = kept {
                self.history.summarize(
                    self.history.last_traditional_input.saturating_add(retained),
                    kept,
                    self.scenario.summary,
                );
                self.result.traditional_compactions += 1;
                self.traditional_edited = true;
                self.receipts.modeled_cache.clear();
            }
            // Recover quantities for FUTURE pricing, never the omitted primary
            // or summary fee. Their monetary exclusion is immutable.
            self.history.pending_retention = None;
        }
        self.receipts.last_appended_input = previous
            .as_ref()
            .filter(|_| compatible)
            .map(|a| native.input_tokens - a.input);
        let mut traditional = self.history.input(native.input_tokens, self.restored);

        // Native totals decide whether a hypothetical summary is needed.
        // Count its protected slice only when that decision needs the floor.
        let protected = if self.scenario.valid() && !history_compatible && traditional > budget {
            crate::savings::protected_prompt_tokens(body).unwrap_or(0)
        } else {
            0
        };
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
        if !self.receipts.cache_initialized
            && !self.traditional_edited
            && mode == carry::models::ScenarioCacheMode::Implicit
            && ttl > 0
        {
            let inherited = self.result.cache_initializations.prefix(
                traditional,
                minimum,
                self.history
                    .is_identical()
                    .then_some(native.cached_input_tokens),
            );
            cache.inherit(cache_scope.clone(), at, ttl, inherited);
        }
        self.receipts.cache_initialized = true;
        if let Some(prior_at) = recovered_cache_request_at
            && mode == carry::models::ScenarioCacheMode::Implicit
            && minimum.is_some_and(|min| traditional >= min)
        {
            let read_refresh = resolved
                .and_then(|r| r.scenario_cache_policy.as_ref())
                .and_then(|p| p.read_refresh_seconds(model, body));
            cache.requested(
                cache_scope.clone(),
                prior_at,
                ttl,
                traditional,
                read_refresh,
            );
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
        // A successful known INPUT evolves the modeled cache even when its
        // OUTPUT retention is unknown. Capture eligibility before this request
        // writes/refreshes it; never silently turn a quantity gap into coldness.
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
        let Some(retained_output) = retained_output else {
            self.result.coverage.unmodeled_calls += 1;
            self.result.excluded_unpriced_calls += 1;
            self.result.initial_compactions += u64::from(initialized_now);
            self.history.last_traditional_input = traditional;
            self.history.defer_retention(
                body,
                &response["output"],
                self.restored,
                projection_fixed_tokens(body),
            );
            self.history
                .observe(contract, items, native.input_tokens, at);
            self.receipts.anchor = Some(Anchor {
                configuration: config,
                prefix: digest(items),
                items: items.len(),
                input: native.input_tokens,
                at,
            });
            self.carry_edited = false;
            self.traditional_edited = false;
            return;
        };
        // Identical execution has one paid contribution. Only a genuinely
        // different history asks the cache/tariff forecast a hypothetical price.
        // Only an identical initial prompt inherits real cache evidence. After
        // divergence the modeled branch evolves independently of native hits.
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
        let fixed = if self.scenario.valid()
            && total
                > self
                    .scenario
                    .context_window
                    .saturating_sub(self.scenario.reserve)
        {
            projection_fixed_tokens(body)
        } else {
            0
        };
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
            let estimated = estimated_quantity || kept.is_some() || initialized_now;
            self.result.coverage.estimated_quantity_calls += u64::from(estimated);
            self.result.coverage.measured_quantity_calls += u64::from(!estimated);
        } else {
            self.result.excluded_unpriced_calls += 1;
            if cached.is_none() {
                self.result.coverage.unknown_cache_requests += 1;
            }
        }
        self.history.last_traditional_input = traditional;
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
