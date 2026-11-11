//! Shared, content-free request evidence. No hypothetical totals are persisted.
//! Stats replays the same transition engine used by native accounting tests.
use super::*;
use sha2::{Digest, Sha256};

fn hash<T: Serialize + ?Sized>(value: &T) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}
fn references(items: &[Value]) -> Vec<String> {
    crate::openai::semantic_prompt_input(items)
        .iter()
        .map(hash)
        .collect()
}

/// Constant-size source witnesses, not retained semantic vectors. Prefix
/// digests are of the actual current input at the previous primary's frontier.
/// They permit the same replay/cache checks after the semantic suffix rolls off.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct PromptWitness {
    items: usize,
    wire: String,
    semantic: String,
    prior_prefix: Option<(usize, String, String)>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct PrimaryFacts {
    pub configuration: String,
    pub contract: HistoryContract,
    pub model: String,
    pub resolved: Option<ResolvedModel>,
    pub at: u64,
    // Prefix references point to the preceding observed primary input. These
    // are storage links only, never history/cache/removal authority.
    pub reused_input: usize,
    pub input: Option<Vec<String>>,
    pub semantic_input: Option<Vec<String>>,
    pub semantic_output: Option<Vec<String>>,
    pub native: Option<Usage>,
    pub cost: Option<f64>,
    pub billable: bool,
    pub retained: Option<u64>,
    pub retained_visible: Option<u64>,
    pub retained_opaque: Option<u64>,
    pub retained_member: Option<u64>,
    pub estimated_retained: bool,
    pub unsupported_opaque: bool,
    pub allowance: Option<u64>,
    pub fixed: Option<u64>,
    pub protected: Option<u64>,
    pub cache_scope: String,
    pub cache_mode: carry::models::ScenarioCacheMode,
    pub read_refresh: Option<u64>,
    #[serde(default)]
    pub native_summary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<PromptWitness>,
}
impl PrimaryFacts {
    pub(super) fn input_len(&self) -> Option<usize> {
        self.input
            .as_ref()
            .map(Vec::len)
            .or_else(|| self.prompt.as_ref().map(|p| p.items))
    }
    pub(super) fn prompt_digest(&self, semantic: bool, prefix: usize) -> Option<String> {
        let items = if semantic {
            &self.semantic_input
        } else {
            &self.input
        };
        if let Some(items) = items {
            return (prefix <= items.len()).then(|| hash(&items[..prefix]));
        }
        let prompt = self.prompt.as_ref()?;
        if prefix == prompt.items {
            return Some(
                if semantic {
                    &prompt.semantic
                } else {
                    &prompt.wire
                }
                .clone(),
            );
        }
        let (len, wire, sem) = prompt.prior_prefix.as_ref()?;
        (*len == prefix).then(|| if semantic { sem } else { wire }.clone())
    }
    fn numeric(&self, prior_items: usize) -> Self {
        let mut facts = self.clone();
        facts.prompt =
            self.input
                .as_ref()
                .zip(self.semantic_input.as_ref())
                .map(|(wire, semantic)| PromptWitness {
                    items: wire.len(),
                    wire: hash(wire),
                    semantic: hash(semantic),
                    prior_prefix: (prior_items <= wire.len() && prior_items <= semantic.len())
                        .then(|| {
                            (
                                prior_items,
                                hash(&wire[..prior_items]),
                                hash(&semantic[..prior_items]),
                            )
                        }),
                });
        facts.input = None;
        facts.semantic_input = None;
        facts.semantic_output = None;
        facts.reused_input = 0;
        facts
    }
    pub(super) fn extract(
        primary: PrimaryObservation<'_>,
        member: Option<(usize, usize, u64)>,
        scenario: Scenario,
        restoration: Option<i128>,
    ) -> Self {
        let PrimaryObservation {
            body,
            response,
            route,
            at,
            standard,
            resolved,
            cost,
            measurements,
        } = primary;
        let model = body["model"].as_str().unwrap_or("").to_owned();
        let extra = if crate::savings::prompt_has_opaque(&response["output"]) {
            measurements.and_then(|m| m.retained_reasoning_extra(&response["output"]))
        } else {
            Some((0, 0))
        };
        let visible = receipts::retained_output_tokens(&response["output"]);
        let retained = visible
            .zip(extra)
            .and_then(|(visible, (extra, _))| visible.checked_add(extra));
        let allowance = carry::models::output_allowance(
            body["max_output_tokens"].as_u64(),
            resolved.map_or(scenario.reserve, |r| u64::from(r.ordinary.output_reserve)),
        );
        let budget = scenario
            .initial_input_budget(
                allowance,
                resolved.and_then(|r| r.capacity_tokens.map(u64::from)),
            )
            .min(scenario.context_window.saturating_sub(scenario.reserve));
        let native = receipts::receipt(&response["usage"]);
        let needs_floor = native.as_ref().zip(restoration).zip(retained).is_none_or(
            |((native, restoration), output)| {
                (native.input_tokens as i128)
                    .saturating_add(restoration)
                    .saturating_add(output as i128)
                    > budget as i128
            },
        );
        let retained_member = member.and_then(|(start, end, visible)| {
            let m = measurements?;
            let items = body["input"].as_array()?;
            if start > end || end > items.len() {
                return None;
            }
            let mut without = body.clone();
            without["input"].as_array_mut()?.drain(start..end);
            let quantity = m.accounting_restoration(body, &without, route, resolved)?;
            let input_visible = items[start..end].iter().try_fold(0u64, |sum, item| {
                sum.checked_add(m.visible_text_tokens(item))
            })?;
            visible.checked_add(
                u64::try_from(quantity.tokens)
                    .ok()?
                    .checked_sub(input_visible)?,
            )
        });
        // Count only the protected/fixed semantic floor, never the full native
        // input. These quantities are shared prompt evidence, not forecasts.
        Self {
            prompt: None,
            configuration: receipts::configuration(body, route, resolved),
            contract: HistoryContract::extract(body, route, resolved, Scenario::default()),
            model: model.clone(),
            resolved: resolved.cloned(),
            at,
            reused_input: 0,
            input: body["input"]
                .as_array()
                .map(|items| items.iter().map(hash).collect()),
            semantic_input: body["input"].as_array().map(|items| references(items)),
            semantic_output: response["output"].as_array().map(|items| references(items)),
            native,
            cost,
            billable: standard
                && !receipts::has_unpriced_modalities(body)
                && !has_native_modalities(&response["output"])
                && !["input_tokens_details", "output_tokens_details"]
                    .iter()
                    .any(|f| {
                        response["usage"][*f]["audio_tokens"]
                            .as_u64()
                            .is_some_and(|n| n > 0)
                    }),
            retained,
            retained_visible: visible,
            retained_opaque: crate::proxy_quantity::retained_reasoning_count(&response["output"]),
            retained_member,
            estimated_retained: extra.is_some_and(|(_, n)| n > 0),
            unsupported_opaque: crate::savings::prompt_has_opaque(body)
                && !crate::proxy_quantity::accounting_retirement_supported(body),
            allowance: body["max_output_tokens"].as_u64(),
            fixed: crate::savings::fixed_prompt_projection(body).map(|value| {
                measurements.map_or_else(|| token_units(&value), |m| m.visible_text_tokens(&value))
            }),
            protected: needs_floor
                .then(|| crate::savings::protected_prompt_tokens(body))
                .flatten(),
            cache_scope: crate::proxy_cache::modeled_scope(
                body,
                &model,
                (route, resolved.map(|r| &r.route)),
            ),
            cache_mode: request_cache_mode(body, &model, resolved),
            native_summary: response["output"]
                .as_array()
                .is_some_and(|out| out.iter().any(|item| item["type"] == "compaction")),
            read_refresh: resolved
                .and_then(|r| r.scenario_cache_policy.as_ref())
                .and_then(|p| p.read_refresh_seconds(&model, body)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum Observation {
    Dispatch {
        request_id: String,
        selection: String,
        continuation: Option<String>,
        omitted_members: u64,
        newly_omitted_members: u64,
        forced: bool,
        input_reduction_estimate: Option<i128>,
    },
    Primary {
        facts: Box<PrimaryFacts>,
        restoration: Option<crate::proxy_quantity::RestorationEstimate>,
        edited: bool,
    },
    Reviewer {
        usage: Value,
        standard: bool,
        cost: Option<f64>,
    },
    FailedPrimary,
    FailedReviewer,
    Unmodeled {
        cost: Option<f64>,
    },
    Gap {
        calls: u64,
    },
    Boundary,
}

/// Derived from dispatched selections, independently of quantity availability.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RewriteCount {
    pub dispatched: u64,
    pub forced: u64,
    pub prior_unknown: bool,
}

/// One owner for successful receipts, failed terminal outcomes and input
/// quantities. Hash references retain chronology without another prompt journal.
/// A bounded suffix is replayable; a missing prefix is explicitly unavailable.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestEvidence {
    format: EvidenceFormat,
    pub scenario: Scenario,
    observations: Vec<Observation>,
    #[serde(default)]
    prior_untracked: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    historical_success_prefix: Option<HistoricalSuccessPrefix>,
    #[serde(skip)]
    historical_prefix_validated: bool,
    #[serde(skip)]
    restoration: Option<crate::proxy_quantity::RestorationEstimate>,
    #[serde(skip)]
    edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive: Option<archive::Watermark>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    numeric_pending: Vec<Observation>,
    // Previous full primary frontier, independent of semantic suffix eviction.
    // A source item count only; prefix digests are captured from the next input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    numeric_primary_items: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    numeric_lost: bool,
    #[serde(skip)]
    archive_access: Option<archive::Access>,
    // The actual terminal event before semantic prefix compression. Only its
    // fixed-size numeric witness is needed by mandatory paid-only publication.
    #[serde(skip)]
    terminal_numeric: Option<Observation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comparison_context: Option<archive::ComparisonContext>,
}
#[path = "proxy_evidence_archive.rs"]
mod archive;
pub(crate) use archive::ComparisonContext;

#[derive(Clone, Debug, Deserialize, Serialize)]
enum EvidenceFormat {
    #[serde(rename = "carry-request-evidence-v1")]
    Current,
}
impl Default for RequestEvidence {
    fn default() -> Self {
        Self {
            format: EvidenceFormat::Current,
            scenario: Scenario::default(),
            observations: Vec::new(),
            prior_untracked: 0,
            historical_success_prefix: None,
            historical_prefix_validated: false,
            restoration: Some(crate::proxy_quantity::RestorationEstimate {
                tokens: 0,
                measured: true,
                estimated_opaque_items: 0,
            }),
            edited: false,
            archive: None,
            numeric_pending: Vec::new(),
            numeric_primary_items: None,
            numeric_lost: false,
            archive_access: None,
            terminal_numeric: None,
            comparison_context: None,
        }
    }
}
/// Immutable, externally verified successful-money prefix. Its frozen strict
/// reference/provenance is retained but never used as current quantity authority.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoricalSuccessPrefix {
    pub source_sha256: String,
    pub owner: String,
    pub history_namespace: String,
    pub primary_attempts: u64,
    pub reviewer_attempts: u64,
    pub estimate: Estimate,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoricalSuccessSeed {
    format: HistoricalSeedFormat,
    pub prefixes: Vec<HistoricalSuccessPrefix>,
}
#[derive(Deserialize)]
enum HistoricalSeedFormat {
    #[serde(rename = "carry-historical-success-seed-v1")]
    Current,
}
impl HistoricalSuccessSeed {
    pub fn validate(&self) -> bool {
        let _ = &self.format;
        self.prefixes.len() <= 9
            && self.prefixes.iter().enumerate().all(|(i, p)| {
                self.prefixes[..i]
                    .iter()
                    .all(|earlier| earlier.owner != p.owner)
            })
    }
}

pub(crate) struct SuccessWindow<'a> {
    pub owner: &'a str,
    pub namespace: Option<&'a str>,
    pub primary_attempts: u64,
    pub reviewer_attempts: u64,
    pub primary_successes: u64,
}

impl RequestEvidence {
    /// Called only after restoring the authoritative Session/index owner. No
    /// paid ledger or replay observations are changed by this one-time import.
    pub(crate) fn bind_success_prefix(
        &mut self,
        window: SuccessWindow<'_>,
        seed: Option<HistoricalSuccessPrefix>,
    ) -> Result<(), &'static str> {
        let already_bound = self.historical_success_prefix.is_some();
        if let Some(seed) = &seed
            && let Some(existing) = &self.historical_success_prefix
            && serde_json::to_value(existing).unwrap() != serde_json::to_value(seed).unwrap()
        {
            return Err("conflicting historical successful prefix");
        }
        self.historical_prefix_validated = false;
        let Some(prefix) = self.historical_success_prefix.as_ref().or(seed.as_ref()) else {
            return Ok(());
        };
        let e = &prefix.estimate;
        let c = &e.coverage;
        let digest = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let (mut primary, mut reviewer, mut successes) = (0u64, 0u64, 0u64);
        for observation in &self.observations {
            match observation {
                Observation::Primary { .. } | Observation::Unmodeled { .. } => {
                    primary += 1;
                    successes += 1;
                }
                Observation::Reviewer { .. } | Observation::FailedReviewer => reviewer += 1,
                Observation::FailedPrimary => primary += 1,
                Observation::Boundary | Observation::Gap { .. } | Observation::Dispatch { .. } => {}
            }
        }
        let money = |cost: Option<f64>, calls: u64, unknown: u64| {
            unknown <= calls
                && cost.is_none_or(|v| v.is_finite() && v >= 0.0 && (calls > 0 || v == 0.0))
        };
        // A first import must cover the entire missing window. An immutable
        // proof already bound to this owner can survive suffix retention loss,
        // but proves only its original segment, not the newly missing money.
        let uncovered = prefix
            .primary_attempts
            .checked_add(prefix.reviewer_attempts)
            .and_then(|covered| self.prior_untracked.checked_sub(covered));
        let lost_primary = window
            .primary_attempts
            .checked_sub(primary)
            .and_then(|missing| missing.checked_sub(prefix.primary_attempts));
        let lost_reviewer = window
            .reviewer_attempts
            .checked_sub(reviewer)
            .and_then(|missing| missing.checked_sub(prefix.reviewer_attempts));
        let lost_successes = window
            .primary_successes
            .checked_sub(successes)
            .and_then(|missing| missing.checked_sub(c.primary_responses));
        let window_matches = match (uncovered, lost_primary, lost_reviewer, lost_successes) {
            (Some(gap), Some(primary), Some(reviewer), Some(successes)) => {
                primary.checked_add(reviewer) == Some(gap)
                    && successes <= primary
                    && (already_bound || gap == 0)
            }
            _ => false,
        };
        if !digest(&prefix.source_sha256)
            || !digest(&prefix.owner)
            || prefix.owner != window.owner
            || prefix.history_namespace.is_empty()
            || Some(prefix.history_namespace.as_str()) != window.namespace
            || !window_matches
            || self.prior_untracked == 0
            || c.primary_responses > prefix.primary_attempts
            || c.reviewer_responses > prefix.reviewer_attempts
            || c.prior_untracked_calls != 0
            || c.failed_calls
                != prefix.primary_attempts - c.primary_responses + prefix.reviewer_attempts
                    - c.reviewer_responses
            || e.matched_primary_responses > c.primary_responses
            || !money(
                e.observed_primary_cost,
                c.primary_responses,
                e.unavailable_primary_cost_calls,
            )
            || !money(
                e.observed_reviewer_cost,
                c.reviewer_responses,
                e.unavailable_reviewer_cost_calls,
            )
        {
            return Err("historical successful prefix does not match accounting window");
        }
        if !already_bound {
            self.historical_success_prefix = seed;
        }
        self.historical_prefix_validated = true;
        Ok(())
    }

    /// Upper envelope of one mandatory terminal append, its numeric archive
    /// suffix and the publication-failure boundary. No provider response is
    /// invented or persisted: this is shape/size admission before transport.
    pub(crate) fn completion_bound(
        &self,
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
        primary: bool,
    ) -> std::io::Result<u64> {
        let event = if primary {
            // Source/prompt/configuration identities are fixed-width digests.
            // Their CONTENT is irrelevant to size admission: do not hash or
            // tokenize another copy of the actual (possibly large) prompt.
            let shape_body = json!({"model":body["model"]});
            let mut facts = PrimaryFacts::extract(
                PrimaryObservation {
                    body: &shape_body,
                    response: &Value::Null,
                    route,
                    at: 0,
                    standard: true,
                    resolved,
                    cost: Some(0.0),
                    measurements: None,
                },
                None,
                self.scenario,
                Some(0),
            );
            facts.input = None;
            facts.semantic_input = None;
            facts.semantic_output = None;
            facts.prompt = Some(PromptWitness {
                items: 0,
                wire: "0".repeat(64),
                semantic: "0".repeat(64),
                prior_prefix: Some((0, "0".repeat(64), "0".repeat(64))),
            });
            facts.native = Some(Usage::default());
            facts.retained = Some(0);
            facts.retained_visible = Some(0);
            facts.retained_opaque = Some(0);
            facts.retained_member = Some(0);
            facts.allowance = Some(0);
            facts.fixed = Some(0);
            facts.protected = Some(0);
            facts.read_refresh = Some(0);
            // Implicit and Disabled are the longest serialized mode variants.
            facts.cache_mode = carry::models::ScenarioCacheMode::Disabled;
            Observation::Primary {
                facts: Box::new(facts),
                restoration: Some(crate::proxy_quantity::RestorationEstimate {
                    tokens: 0,
                    measured: true,
                    estimated_opaque_items: 0,
                }),
                edited: false,
            }
        } else {
            Observation::Reviewer {
                usage: receipts::review_usage_shape(),
                standard: true,
                cost: Some(0.0),
            }
        };
        crate::proxy::settlement::size_bound(&json!({
            "observations":[&event, Observation::Boundary],
            "numeric_pending":[&event, Observation::Boundary],
            "numeric_primary_items":0,
            "prior_untracked":0,
            "numeric_lost":false,
            "archive":self.archive,
        }))
    }

    /// Keep the actual new receipt's bounded witness without dropping any prior
    /// observations. Successful semantic publication still keeps full hashes;
    /// paid-only publication deliberately grants no new semantic authority.
    pub(crate) fn retain_numeric_terminal(&mut self) {
        if let Some(event @ Observation::Primary { .. }) = &self.terminal_numeric
            && let Some(target) = self
                .observations
                .iter_mut()
                .rev()
                .find(|event| matches!(event, Observation::Primary { .. }))
        {
            *target = event.clone();
        }
    }

    const MAX_OBSERVATIONS: usize = 4096;
    const MAX_REFERENCES: usize = 65536;

    fn expand(facts: &PrimaryFacts, wire: &mut Vec<String>, semantic: &mut Vec<String>) -> bool {
        let Some((next_wire, next_semantic)) =
            facts.input.as_ref().zip(facts.semantic_input.as_ref())
        else {
            wire.clear();
            semantic.clear();
            return false;
        };
        let prefix = facts.reused_input;
        if prefix > wire.len() || prefix > semantic.len() || next_wire.len() != next_semantic.len()
        {
            wire.clear();
            semantic.clear();
            return false;
        }
        wire.truncate(prefix);
        semantic.truncate(prefix);
        wire.extend(next_wire.iter().cloned());
        semantic.extend(next_semantic.iter().cloned());
        true
    }
    fn last_prompt(&self) -> (Vec<String>, Vec<String>) {
        let (mut wire, mut semantic) = (Vec::new(), Vec::new());
        for observation in &self.observations {
            if let Observation::Primary { facts, .. } = observation {
                Self::expand(facts, &mut wire, &mut semantic);
            }
        }
        (wire, semantic)
    }
    fn discard_prefix(&mut self) {
        self.prior_untracked += self
            .observations
            .iter()
            .filter(|observation| {
                !matches!(
                    observation,
                    Observation::Boundary | Observation::Dispatch { .. }
                )
            })
            .count() as u64;
        self.observations.clear();
    }
    fn append(&mut self, mut observation: Observation) {
        let numeric = match &observation {
            Observation::Primary {
                facts,
                restoration,
                edited,
            } => Some(Observation::Primary {
                facts: Box::new(
                    facts.numeric(
                        self.numeric_primary_items
                            .unwrap_or_else(|| self.last_prompt().0.len()),
                    ),
                ),
                restoration: *restoration,
                edited: *edited,
            }),
            _ => None,
        };
        if numeric.is_some() {
            self.terminal_numeric = numeric.clone();
        }
        if self.archive_access.is_some() {
            self.start_numeric();
            self.queue_numeric(numeric.unwrap_or_else(|| observation.clone()));
        }
        if self.observations.len() == Self::MAX_OBSERVATIONS {
            self.discard_prefix();
        }
        if let Observation::Primary { facts, .. } = &mut observation {
            let retained = self
                .observations
                .iter()
                .map(|observation| match observation {
                    Observation::Primary { facts, .. } => {
                        facts.input.as_ref().map_or(0, Vec::len) * 2
                            + facts.semantic_output.as_ref().map_or(0, Vec::len)
                    }
                    _ => 0,
                })
                .sum::<usize>();
            let (prior_wire, prior_semantic) = self.last_prompt();
            let prefix = facts
                .input
                .as_ref()
                .zip(facts.semantic_input.as_ref())
                .map_or(0, |(wire, semantic)| {
                    wire.iter()
                        .zip(semantic)
                        .zip(prior_wire.iter().zip(&prior_semantic))
                        .take_while(|((wire, semantic), (prior_wire, prior_semantic))| {
                            wire == prior_wire && semantic == prior_semantic
                        })
                        .count()
                });
            let full = facts.input.as_ref().map_or(0, Vec::len) * 2
                + facts.semantic_output.as_ref().map_or(0, Vec::len);
            if retained + full.saturating_sub(prefix * 2) > Self::MAX_REFERENCES {
                self.discard_prefix();
            } else if let Some((wire, semantic)) =
                facts.input.as_mut().zip(facts.semantic_input.as_mut())
            {
                wire.drain(..prefix);
                semantic.drain(..prefix);
                facts.reused_input = prefix;
            }
            if full > Self::MAX_REFERENCES {
                facts.input = None;
                facts.semantic_input = None;
                facts.semantic_output = None;
                facts.reused_input = 0;
            }
        }
        self.observations.push(observation);
    }
    pub fn snapshot(&self) -> Estimate {
        let mut strict = self.replay_snapshot(false);
        let numeric = self.replay_snapshot(true);
        if self.archive.is_some() && !numeric.archive_unavailable {
            // Actual successful-money population survives semantic retention.
            // Strict matched quantities remain suffix diagnostics, never scaled.
            strict.observed_primary_cost = numeric.observed_primary_cost;
            strict.observed_reviewer_cost = numeric.observed_reviewer_cost;
            strict.unavailable_primary_cost_calls = numeric.unavailable_primary_cost_calls;
            strict.unavailable_reviewer_cost_calls = numeric.unavailable_reviewer_cost_calls;
            strict.coverage.primary_responses = numeric.coverage.primary_responses;
            strict.coverage.reviewer_responses = numeric.coverage.reviewer_responses;
            strict.coverage.failed_calls = numeric.coverage.failed_calls;
            strict.coverage.prior_untracked_calls = numeric.coverage.prior_untracked_calls;
            strict.excluded_failed_calls = numeric.excluded_failed_calls;
        }
        strict.chronological = numeric.chronological;
        strict.rewrites = numeric.rewrites;
        if numeric.archive_unavailable || self.numeric_lost {
            strict.chronological = None;
            strict.archive_unavailable = true;
        }
        strict
    }

    fn replay_snapshot(&self, infer: bool) -> Estimate {
        let mut inferred_history = false;
        let mut inferred_calls = 0;
        let mut inferred_cost = 0.0;
        let mut last_restoration = Some(crate::proxy_quantity::RestorationEstimate {
            tokens: 0,
            measured: true,
            estimated_opaque_items: 0,
        });
        let mut replay = Replay {
            scenario: self.scenario,
            chronological: infer,
            ..Replay::default()
        };
        let prior_untracked = if infer && self.archive.is_some() {
            self.numeric_prior_untracked()
        } else {
            self.prior_untracked
        };
        let prefix = self.historical_success_prefix.as_ref().filter(|prefix| {
            self.historical_prefix_validated
                && prefix
                    .primary_attempts
                    .checked_add(prefix.reviewer_attempts)
                    == Some(prior_untracked)
        });

        let mut rewrites = RewriteCount {
            prior_unknown: prior_untracked > 0,
            ..Default::default()
        };
        let mut dispatched = false;
        let (mut wire, mut semantic) = (Vec::new(), Vec::new());
        let mut observe = |observation: Option<&Observation>, context_complete: bool| {
            let Some(observation) = observation else {
                if self.comparison_context.is_some() {
                    replay.begin_owned_window(context_complete);
                    rewrites = RewriteCount {
                        prior_unknown: prior_untracked > 0,
                        ..Default::default()
                    };
                    dispatched = false;
                    inferred_calls = 0;
                    inferred_cost = 0.0;
                }
                replay.prior_untracked(if prefix.is_some() { 0 } else { prior_untracked });
                return;
            };
            match observation {
                Observation::Dispatch {
                    newly_omitted_members,
                    forced,
                    ..
                } => {
                    dispatched = true;
                    if *newly_omitted_members > 0 {
                        rewrites.dispatched = rewrites.dispatched.saturating_add(1);
                        rewrites.forced = rewrites.forced.saturating_add(u64::from(*forced));
                    }
                }
                Observation::Primary {
                    facts,
                    restoration,
                    edited,
                } => {
                    rewrites.prior_unknown |= !dispatched;
                    dispatched = false;
                    let mut expanded = facts.as_ref().clone();
                    if Self::expand(facts, &mut wire, &mut semantic) {
                        expanded.input = Some(wire.clone());
                        expanded.semantic_input = Some(semantic.clone());
                    } else {
                        expanded.input = None;
                        expanded.semantic_input = None;
                    }
                    let mut projection = *restoration;
                    if infer {
                        if expanded.retained.is_none()
                            && (expanded.native_summary
                                || expanded.retained_opaque.is_some_and(|n| n > 0))
                            && let Some(native) = &expanded.native
                        {
                            // A conditional quantity estimate, never a measured
                            // retained-output receipt or semantic authority.
                            expanded.retained = Some(
                                native
                                    .output_tokens
                                    .max(expanded.retained_visible.unwrap_or(0)),
                            );
                            expanded.estimated_retained = true;
                            inferred_history = true;
                        }
                        if projection.is_none() {
                            projection = last_restoration;
                            inferred_history = true;
                        } else {
                            last_restoration = projection;
                        }
                    }
                    let before = replay.result.traditional_cost;
                    replay.projected_evidence(projection, *edited);
                    replay.observed_facts(&expanded);
                    inferred_history |= replay.inferred_continuity;
                    if infer && inferred_history {
                        inferred_calls += 1;
                        inferred_cost += replay.result.traditional_cost - before;
                    }
                }
                Observation::Reviewer {
                    usage,
                    standard,
                    cost,
                } => replay.observed_reviewer(usage, *standard, *cost),
                Observation::FailedPrimary => {
                    rewrites.prior_unknown |= !dispatched;
                    dispatched = false;
                    replay.failed_call();
                }
                Observation::FailedReviewer => replay.failed_reviewer_call(),
                Observation::Unmodeled { cost } => {
                    rewrites.prior_unknown |= !dispatched;
                    dispatched = false;
                    replay.unmodeled_call(*cost);
                }
                Observation::Gap { calls } => {
                    rewrites.prior_unknown |= *calls > 0;
                    replay.prior_untracked(*calls);
                }
                Observation::Boundary => replay.reset_epoch(),
            }
        };
        let context_complete = self
            .scan_comparison_context(|event| observe(Some(event), true))
            .is_ok();
        observe(None, context_complete);
        let archive_complete = if infer && self.archive.is_some() {
            self.scan_numeric(|event| observe(Some(event), true))
                .is_ok()
        } else {
            for observation in &self.observations {
                observe(Some(observation), true);
            }
            true
        } && context_complete;
        let mut estimate = replay.snapshot();
        estimate.rewrites = if archive_complete && !self.numeric_lost {
            rewrites
        } else {
            RewriteCount {
                prior_unknown: true,
                ..Default::default()
            }
        };
        estimate.archive_unavailable = !archive_complete;
        if infer
            && archive_complete
            && prior_untracked == 0
            && estimate.coverage.prior_untracked_calls == 0
            && estimate.matched_primary_responses == estimate.coverage.primary_responses
        {
            estimate.chronological = Some(ChronologicalEstimate {
                traditional_cost: estimate.traditional_cost,
                primary_responses: estimate.coverage.primary_responses,
                inferred_primary_responses: inferred_calls,
                inferred_traditional_cost: inferred_cost,
                initial_compactions: estimate.initial_compactions,
                traditional_compactions: estimate.traditional_compactions,
                summary_cost: estimate.summary_cost,
                quantity_assumptions: "native_total_input;known_retention_else_max_visible_and_billed_output_proxy_for_reasoning_or_native_summary;missing_restoration_uses_last_known_or_zero_at_tracking_start;downward_canonical_rebase_holds_prior_quantity_with_zero_unknown_append;recognition_and_settings_changes_cool_cache_not_retired_history;same_outputs_and_tool_sequence;not_native_quantity_or_removal_authority".into(),
            });
        }
        if let Some(prefix) = prefix {
            let old = &prefix.estimate;
            let add_money = |current: Option<f64>, old: Option<f64>, calls: u64| {
                current
                    .zip(if calls == 0 { Some(0.0) } else { old })
                    .map(|(a, b)| a + b)
            };
            estimate.observed_primary_cost = add_money(
                estimate.observed_primary_cost,
                old.observed_primary_cost,
                old.coverage.primary_responses,
            );
            estimate.observed_reviewer_cost = add_money(
                estimate.observed_reviewer_cost,
                old.observed_reviewer_cost,
                old.coverage.reviewer_responses,
            );
            estimate.unavailable_primary_cost_calls += old.unavailable_primary_cost_calls;
            estimate.unavailable_reviewer_cost_calls += old.unavailable_reviewer_cost_calls;
            estimate.coverage.primary_responses += old.coverage.primary_responses;
            estimate.coverage.reviewer_responses += old.coverage.reviewer_responses;
            estimate.coverage.failed_calls += old.coverage.failed_calls;
            estimate.excluded_failed_calls += old.coverage.failed_calls;
            // The frozen strict reference is NOT replay/quantity authority.
            // Preserve successful money, but do not fabricate chronology from
            // its aggregate cost ratio when the request inputs were lost.
            estimate.coverage.unmodeled_calls += old.coverage.primary_responses;
            estimate.excluded_unpriced_calls += old.coverage.primary_responses;
        }
        estimate
    }
    pub fn prior_untracked(&mut self, calls: u64) {
        if calls > 0 && self.archive_access.is_some() {
            self.start_numeric();
            self.queue_numeric(Observation::Gap { calls });
        }
        self.prior_untracked += calls;
    }
    pub fn failed_call(&mut self) {
        self.append(Observation::FailedPrimary);
    }
    /// Record only the primary selection submitted at the dispatch boundary.
    /// The planner supplies the currently reauthorized prior projection plus
    /// appends. Old omissions are never semantic authority or new cleanups.
    pub fn dispatched_selection(
        &mut self,
        request_id: &str,
        omitted: &std::collections::HashSet<u64>,
        continuation: Option<&std::collections::HashSet<u64>>,
        forced: bool,
        input_reduction_estimate: Option<i128>,
    ) {
        let identity = |ids: &std::collections::HashSet<u64>| {
            hash(
                &ids.iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
            )
        };
        self.append(Observation::Dispatch {
            request_id: request_id.into(),
            selection: identity(omitted),
            continuation: continuation.map(identity),
            omitted_members: omitted.len() as u64,
            newly_omitted_members: omitted
                .iter()
                .filter(|id| continuation.is_none_or(|prior| !prior.contains(id)))
                .count() as u64,
            forced,
            input_reduction_estimate,
        });
    }
    pub fn failed_reviewer_call(&mut self) {
        self.append(Observation::FailedReviewer);
    }
    pub fn reset_epoch(&mut self) {
        self.append(Observation::Boundary);
        self.restoration = Some(crate::proxy_quantity::RestorationEstimate {
            tokens: 0,
            measured: true,
            estimated_opaque_items: 0,
        });
        self.edited = false;
    }
    pub(crate) fn projected_evidence(
        &mut self,
        restoration: Option<crate::proxy_quantity::RestorationEstimate>,
        edited: bool,
    ) {
        self.restoration = restoration;
        self.edited = edited;
    }
    pub fn reviewer_usage_settled(&mut self, usage: &Value, standard: bool, cost: Option<f64>) {
        self.append(Observation::Reviewer {
            usage: receipts::numeric_review_usage(usage),
            standard,
            cost,
        });
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
        let cost = receipts::receipt(&response["usage"])
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
            cost,
            measurements: None,
        });
    }
    pub fn primary_settled(&mut self, primary: PrimaryObservation<'_>) {
        if primary.response["status"] != "completed" {
            self.failed_call();
            return;
        }
        let member = self
            .observations
            .iter()
            .rev()
            .find_map(|observation| match observation {
                Observation::Primary { facts, .. } => Some(Some(facts)),
                Observation::Boundary | Observation::Unmodeled { .. } => Some(None),
                _ => None,
            })
            .flatten()
            .filter(|facts| facts.retained.is_none())
            .and_then(|facts| {
                Some((
                    facts.reused_input + facts.semantic_input.as_ref()?.len(),
                    facts.reused_input
                        + facts.semantic_input.as_ref()?.len()
                        + facts.semantic_output.as_ref()?.len(),
                    facts.retained_visible?,
                ))
            });
        let facts = PrimaryFacts::extract(
            primary,
            member,
            self.scenario,
            self.restoration.map(|e| e.tokens),
        );
        self.append(Observation::Primary {
            facts: Box::new(facts),
            restoration: self.restoration,
            edited: self.edited,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recovery_prefix() -> HistoricalSuccessPrefix {
        HistoricalSuccessPrefix {
            source_sha256: "a".repeat(64),
            owner: "b".repeat(64),
            history_namespace: "synthetic-window".into(),
            primary_attempts: 3,
            reviewer_attempts: 1,
            estimate: Estimate {
                accounting_basis: RECEIPT_BASIS.into(),
                observed_primary_cost: Some(3.0),
                observed_reviewer_cost: Some(0.2),
                matched_primary_responses: 1,
                carry_cost: 2.1,
                traditional_cost: 4.0,
                reviewer_cost: 0.1,
                counterfactual_cache_policy: "frozen-synthetic-policy".into(),
                coverage: Coverage {
                    primary_responses: 2,
                    reviewer_responses: 1,
                    failed_calls: 1,
                    ..Coverage::default()
                },
                ..Estimate::default()
            },
        }
    }
    fn recovery_window() -> SuccessWindow<'static> {
        SuccessWindow {
            owner: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            namespace: Some("synthetic-window"),
            primary_attempts: 4,
            reviewer_attempts: 2,
            primary_successes: 3,
        }
    }
    fn recovery_suffix() -> RequestEvidence {
        let mut evidence = RequestEvidence::default();
        evidence.prior_untracked(4);
        evidence.append(Observation::Unmodeled { cost: Some(4.0) });
        evidence.failed_reviewer_call();
        evidence
    }

    #[test]
    fn historical_success_prefix_is_idempotent_numeric_and_restart_bound() {
        let mut evidence = recovery_suffix();
        let prefix = recovery_prefix();
        let frozen = serde_json::to_value(&prefix).unwrap();
        evidence
            .bind_success_prefix(recovery_window(), Some(prefix.clone()))
            .unwrap();
        let saved = serde_json::to_value(&evidence).unwrap();
        let dashboard = serde_json::to_value(evidence.snapshot().dashboard(false)).unwrap();
        assert!(
            dashboard["whole_window"].is_null(),
            "an aggregate seed cannot reconstruct missing request inputs"
        );
        assert_eq!(dashboard["observed_primary_cost"], 7.0);
        assert_eq!(dashboard["observed_reviewer_cost"], 0.2);
        assert_eq!(dashboard["coverage"]["failed_calls"], 2);
        assert_eq!(dashboard["matched_primary_responses"], 0);
        evidence
            .bind_success_prefix(recovery_window(), Some(prefix))
            .unwrap();
        assert_eq!(serde_json::to_value(&evidence).unwrap(), saved);
        evidence = serde_json::from_value(saved).unwrap();
        assert!(
            evidence.snapshot().dashboard(false).whole_window.is_none(),
            "unbound decode cannot grant proof"
        );
        evidence
            .bind_success_prefix(recovery_window(), None)
            .unwrap();
        assert_eq!(
            serde_json::to_value(evidence.snapshot().dashboard(false)).unwrap(),
            dashboard
        );
        assert_eq!(
            serde_json::to_value(evidence.historical_success_prefix.as_ref().unwrap()).unwrap(),
            frozen
        );
    }

    #[test]
    fn historical_success_prefix_rejects_conflict_fork_and_count_mismatch() {
        let mut evidence = recovery_suffix();
        evidence
            .bind_success_prefix(recovery_window(), Some(recovery_prefix()))
            .unwrap();
        let saved = serde_json::to_value(&evidence).unwrap();
        let mut conflicting = recovery_prefix();
        conflicting.estimate.observed_primary_cost = Some(99.0);
        assert!(
            evidence
                .bind_success_prefix(recovery_window(), Some(conflicting))
                .is_err()
        );
        assert_eq!(serde_json::to_value(&evidence).unwrap(), saved);
        for window in [
            SuccessWindow {
                owner: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                ..recovery_window()
            },
            SuccessWindow {
                namespace: Some("different-window"),
                ..recovery_window()
            },
            SuccessWindow {
                primary_attempts: 5,
                ..recovery_window()
            },
            SuccessWindow {
                primary_successes: 4,
                ..recovery_window()
            },
        ] {
            assert!(evidence.bind_success_prefix(window, None).is_err());
        }
        let p = serde_json::to_string(&recovery_prefix()).unwrap();
        let duplicate =
            format!("{{\"format\":\"carry-historical-success-seed-v1\",\"prefixes\":[{p},{p}]}}");
        assert!(
            !serde_json::from_str::<HistoricalSuccessSeed>(&duplicate)
                .unwrap()
                .validate()
        );
        let duplicate_field = p.replacen("{", "{\"owner\":\"duplicate\",", 1);
        assert!(serde_json::from_str::<HistoricalSuccessPrefix>(&duplicate_field).is_err());
        for malformed in 0..6 {
            let mut candidate = recovery_prefix();
            match malformed {
                0 => candidate.source_sha256 = "A".repeat(64),
                1 => candidate.history_namespace.clear(),
                2 => candidate.estimate.observed_primary_cost = Some(-1.0),
                3 => candidate.estimate.coverage.primary_responses = 4,
                4 => candidate.primary_attempts = u64::MAX,
                _ => candidate.estimate.coverage.failed_calls += 1,
            }
            let mut unbound = recovery_suffix();
            let before = serde_json::to_value(&unbound).unwrap();
            assert!(
                unbound
                    .bind_success_prefix(recovery_window(), Some(candidate))
                    .is_err()
            );
            assert_eq!(serde_json::to_value(&unbound).unwrap(), before);
        }
    }

    #[test]
    fn historical_success_prefix_never_fills_unknown_success_money() {
        for missing in [false, true] {
            let mut evidence = recovery_suffix();
            assert!(evidence.snapshot().dashboard(false).whole_window.is_none());
            let mut prefix = recovery_prefix();
            prefix.estimate.observed_primary_cost = if missing { None } else { Some(3.0) };
            prefix.estimate.unavailable_primary_cost_calls = u64::from(!missing);
            evidence
                .bind_success_prefix(recovery_window(), Some(prefix))
                .unwrap();
            assert!(evidence.snapshot().dashboard(false).whole_window.is_none());
        }
    }

    #[test]
    fn historical_success_prefix_cannot_cover_later_bounded_loss() {
        for reference_bound in [false, true] {
            let mut evidence = recovery_suffix();
            let mut window = recovery_window();
            let prefix = recovery_prefix();
            let frozen = serde_json::to_value(&prefix).unwrap();
            evidence
                .bind_success_prefix(recovery_window(), Some(prefix.clone()))
                .unwrap();
            let body = json!({"model":"gpt-6-luna","input":[{"role":"user","content":"next"}]});
            let response = json!({"status":"completed","output":[],"usage":{"input_tokens":10,"output_tokens":0}});
            let observation = PrimaryObservation {
                body: &body,
                response: &response,
                route: "route",
                at: 100,
                standard: true,
                resolved: None,
                cost: Some(9.0),
                measurements: None,
            };
            if reference_bound {
                evidence.primary_settled(observation);
                window.primary_attempts += 1;
                window.primary_successes += 1;
                let Observation::Primary { facts, .. } = evidence.observations.last_mut().unwrap()
                else {
                    panic!("primary settlement must retain facts");
                };
                // Stopped synthetic suffix at the documented cardinality cap.
                facts.input = Some(vec!["f".repeat(64); RequestEvidence::MAX_REFERENCES / 2]);
                facts.semantic_input = facts.input.clone();
                facts.semantic_output = Some(Vec::new());
                facts.reused_input = 0;
            } else {
                while evidence.observations.len() < RequestEvidence::MAX_OBSERVATIONS {
                    evidence.append(Observation::Unmodeled { cost: Some(1.0) });
                    window.primary_attempts += 1;
                    window.primary_successes += 1;
                }
            }
            evidence.primary_settled(observation); // real append crosses either cap
            window.primary_attempts += 1;
            window.primary_successes += 1;
            assert!(evidence.prior_untracked > 4);
            assert!(evidence.snapshot().dashboard(false).whole_window.is_none());
            assert_eq!(evidence.snapshot().observed_primary_cost, Some(9.0));
            let saved = serde_json::to_value(&evidence).unwrap();
            for seed in [None, Some(prefix.clone())] {
                let mut restored: RequestEvidence = serde_json::from_value(saved.clone()).unwrap();
                restored
                    .bind_success_prefix(SuccessWindow { ..window }, seed)
                    .expect("bounded loss with an already-bound proof must still load");
                assert!(restored.snapshot().dashboard(false).whole_window.is_none());
                assert_eq!(serde_json::to_value(&restored).unwrap(), saved);
                for invalid in [
                    SuccessWindow {
                        owner: "wrong-owner",
                        ..window
                    },
                    SuccessWindow {
                        namespace: Some("wrong-window"),
                        ..window
                    },
                    SuccessWindow {
                        primary_attempts: window.primary_attempts + 1,
                        ..window
                    },
                    SuccessWindow {
                        reviewer_attempts: window.reviewer_attempts + 1,
                        ..window
                    },
                    SuccessWindow {
                        primary_successes: u64::MAX,
                        ..window
                    },
                ] {
                    assert!(restored.bind_success_prefix(invalid, None).is_err());
                }
                let mut conflict = prefix.clone();
                conflict.estimate.observed_primary_cost = Some(99.0);
                assert!(
                    restored
                        .bind_success_prefix(SuccessWindow { ..window }, Some(conflict))
                        .is_err()
                );
                assert_eq!(
                    serde_json::to_value(restored.historical_success_prefix.as_ref().unwrap())
                        .unwrap(),
                    frozen
                );
            }
            // An unbound first import cannot use partial provenance to excuse
            // a count mismatch; only the persisted identical proof may remain.
            let mut unbound = evidence;
            unbound.historical_success_prefix = None;
            assert!(unbound.bind_success_prefix(window, Some(prefix)).is_err());
        }
    }

    #[test]
    fn reviewer_numeric_boundary_preserves_invalid_presence_and_frozen_money() {
        let raw = json!({"input_tokens":100,"output_tokens":7,"cached_input_tokens":3,
            "cache_write_input_tokens":{"private":"sentinel"},
            "cache_creation_input_tokens":"private-sentinel",
            "input_tokens_details":{"cached_tokens":3,"audio_tokens":0,"cache_write_tokens":null,"provider_payload":"private-sentinel"},
            "output_tokens_details":{"audio_tokens":[],"provider_payload":"private-sentinel"},
            "provider_payload":"private-sentinel"});
        let expected = json!({"input_tokens":100,"output_tokens":7,"cached_input_tokens":3,
            "cache_write_input_tokens":-1,"cache_creation_input_tokens":-1,
            "input_tokens_details":{"cached_tokens":3,"audio_tokens":0,"cache_write_tokens":-1},
            "output_tokens_details":{"audio_tokens":-1}});
        let mut evidence = RequestEvidence::default();
        evidence.reviewer_usage_settled(&raw, true, Some(0.125));
        let saved = serde_json::to_value(&evidence).unwrap();
        assert_eq!(saved["observations"][0]["Reviewer"]["usage"], expected);
        assert!(!saved.to_string().contains("private"));
        assert_eq!(receipts::numeric_review_usage(&expected), expected);
        let restored: RequestEvidence = serde_json::from_value(saved).unwrap();
        let before = evidence.snapshot();
        assert_eq!(before.observed_reviewer_cost, Some(0.125));
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(restored.snapshot()).unwrap()
        );
    }

    #[test]
    fn downward_rebase_response_compaction_keeps_fixed_floor() {
        let scenario = Scenario {
            context_window: 4000,
            reserve: 100,
            keep_recent: 1500,
            summary: 100,
        };
        let mut evidence = RequestEvidence {
            scenario,
            ..Default::default()
        };
        let mut body = json!({"model":"gpt-6.1-sol", "max_output_tokens":1,
            "prompt_cache_options":{"mode":"explicit"}, "instructions":" fixed".repeat(1800),
            "input":[{"role":"assistant","content":"old"}]});
        let fixed = crate::savings::fixed_prompt_tokens(&body).unwrap();
        assert!(fixed > scenario.keep_recent && fixed < 2000);
        let output = json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":" result".repeat(500)}]}]);
        let retained = receipts::retained_output_tokens(&output).unwrap();
        assert!(2000 + retained < 3900 && 3700 + retained > 3900);
        let mut expected = 0.0;
        for (i, native) in [2000_u64, 3700, 2000, 2100].into_iter().enumerate() {
            body["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("append {i}")}));
            let response = json!({"status":"completed", "output":if i == 2 {output.clone()} else {json!([])},
                "usage":{"input_tokens":native,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}});
            evidence.primary_settled(PrimaryObservation {
                body: &body,
                response: &response,
                route: "route",
                at: 100 + i as u64,
                standard: true,
                resolved: None,
                cost: Some(native as f64 * 2e-6),
                measurements: None,
            });
            let traditional = match i {
                0 => 2000,
                1 | 2 => 3700,
                _ => fixed + 200 - retained,
            };
            // The canonical receipt includes only raw input; the bridge carries
            // the prior input coordinate. A post-response summary retires the
            // retained output too, so the next native increment subtracts it.
            expected += traditional as f64 * 2e-6;
            if i == 2 {
                expected += ((3700 + retained - fixed) * 2 + 100 * 10) as f64 / 1e6;
            }
        }
        let actual = evidence.snapshot().chronological.unwrap();
        assert_eq!(actual.primary_responses, 4);
        assert_eq!(actual.traditional_compactions, 1);
        let summary_cost = ((3700 + retained - fixed) * 2 + 100 * 10) as f64 / 1e6;
        assert!(
            (actual.summary_cost - summary_cost).abs() < 1e-12,
            "L1 summary retired protected floor: actual={} expected={} fixed={}",
            actual.summary_cost,
            summary_cost,
            fixed
        );
        assert!(
            (actual.traditional_cost - expected).abs() < 1e-12,
            "L1 fixed floor must survive replay-only threshold crossing: actual={} expected={} fixed={} retained={}",
            actual.traditional_cost,
            expected,
            fixed,
            retained
        );
    }

    #[test]
    fn chronological_quantity_fallbacks_are_read_only_and_never_native_authority() {
        let mut evidence = RequestEvidence {
            scenario: Scenario {
                context_window: 4000,
                reserve: 100,
                keep_recent: 1500,
                summary: 100,
            },
            ..Default::default()
        };
        let mut body = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
            "prompt_cache_options":{"mode":"explicit"},
            "input":[{"role":"assistant","content":"old context"}]});
        let mut retired = 0;
        let mut summary = 0;
        let mut expected = 0.0;
        for (i, native) in [1000_u64, 7000, 10000, 13000].into_iter().enumerate() {
            body["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("append {i}")}));
            let opaque = i == 0;
            let response = json!({"status":"completed", "output":if opaque {
                json!([{"type":"reasoning","summary":[],"encrypted_content":"not-input-units"}])
            } else { json!([]) }, "usage":{"input_tokens":native,"output_tokens":if opaque {1000} else {0},
                "input_tokens_details":{"cached_tokens":0}}});
            evidence.projected_evidence(
                (i != 1).then_some(crate::proxy_quantity::RestorationEstimate {
                    tokens: 0,
                    measured: true,
                    estimated_opaque_items: 0,
                }),
                i == 1,
            );
            evidence.primary_settled(PrimaryObservation {
                body: &body,
                response: &response,
                route: "route",
                at: 100 + i as u64,
                standard: true,
                resolved: None,
                cost: Some((native * 2 + if opaque { 10000 } else { 0 }) as f64 / 1e6),
                measurements: None,
            });
            // Independent numeric oracle: same native inputs, one explicit
            // billed-output retention proxy, zero canonical restoration.
            let input = native - retired + summary;
            expected += (input * 2 + if opaque { 10000 } else { 0 }) as f64 / 1e6;
            let total = input + if opaque { 1000 } else { 0 };
            if total > 3900 {
                expected += ((total - 1500) * 2 + 100 * 10) as f64 / 1e6;
                retired += total - 1500 - summary;
                summary = 100;
            }
        }
        let bytes = serde_json::to_vec(&evidence).unwrap();
        let strict = evidence.snapshot();
        assert!(strict.matched_primary_responses < 4);
        let full = strict.chronological.as_ref().unwrap();
        assert_eq!(full.primary_responses, 4);
        assert_eq!(full.initial_compactions, 0);
        assert_eq!(full.traditional_compactions, 3);
        assert!((full.traditional_cost - expected).abs() < 1e-12);
        assert_eq!(serde_json::to_vec(&evidence).unwrap(), bytes);
        let restarted: RequestEvidence = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(restarted.snapshot().chronological).unwrap(),
            serde_json::to_value(strict.chronological).unwrap()
        );
        let Observation::Primary { facts, .. } = &evidence.observations[0] else {
            unreachable!()
        };
        assert!(
            facts.retained.is_none(),
            "the fallback must not manufacture a receipt"
        );
    }

    #[test]
    fn stats_replay_matches_the_transition_engine_across_summary_failure_review_and_restart() {
        for initialized in [false, true] {
            let scenario = Scenario {
                context_window: 4000,
                reserve: 100,
                keep_recent: 1500,
                summary: 100,
            };
            let mut evidence = RequestEvidence {
                scenario,
                ..RequestEvidence::default()
            };
            let mut engine = Replay {
                scenario,
                ..Replay::default()
            };
            let mut body = json!({"model":"gpt-6-luna","input":[{"role":"assistant","content":"private-source-sentinel"}]});
            for (index, input) in [if initialized { 5000 } else { 1000 }, 6000, 9000, 12000]
                .into_iter()
                .enumerate()
            {
                if index > 0 {
                    body["input"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"role":"user","content":format!("append-{index}")}));
                }
                let response = json!({"status":"completed","output":[],"usage":{"input_tokens":input,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}});
                let restoration = Some(crate::proxy_quantity::RestorationEstimate {
                    tokens: if index >= 2 { 200 } else { 0 },
                    measured: index < 2,
                    estimated_opaque_items: u64::from(index >= 2),
                });
                evidence.projected_evidence(restoration, index == 2);
                engine.projected_evidence(restoration, index == 2);
                let observation = PrimaryObservation {
                    body: &body,
                    response: &response,
                    route: "route",
                    at: index as u64 + 100,
                    standard: true,
                    resolved: None,
                    cost: Some(0.00011470000000000001),
                    measurements: None,
                };
                evidence.primary_settled(observation);
                engine.primary_settled(observation);
                evidence.failed_call();
                engine.failed_call();
                evidence.failed_reviewer_call();
                engine.failed_reviewer_call();
                evidence.reviewer_usage_settled(
                    &response["usage"],
                    true,
                    Some(0.00011470000000000001),
                );
                engine.reviewer_usage_settled(
                    &response["usage"],
                    true,
                    Some(0.00011470000000000001),
                );
                let bytes = serde_json::to_vec(&evidence).unwrap();
                assert!(!String::from_utf8_lossy(&bytes).contains("private-source-sentinel"));
                evidence = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    serde_json::to_value(evidence.snapshot()).unwrap(),
                    serde_json::to_value(engine.snapshot()).unwrap(),
                    "strict diagnostics still use the original transition policy"
                );
                let _ = evidence.snapshot();
                let _ = evidence.snapshot();
                assert_eq!(
                    serde_json::to_vec(&evidence).unwrap(),
                    bytes,
                    "stats is a read-only projection"
                );
            }
            assert!(evidence.snapshot().traditional_compactions > 0);
            let primaries = evidence
                .observations
                .iter()
                .filter_map(|observation| match observation {
                    Observation::Primary { facts, .. } => Some(facts),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(primaries.iter().skip(1).all(|facts| facts.reused_input > 0 && facts.input.as_ref().unwrap().len() == 1), "shared prefixes must not become another full prompt journal");
            evidence.reset_epoch();
            engine.reset_epoch();
            assert_eq!(
                serde_json::to_value(evidence.snapshot()).unwrap(),
                serde_json::to_value(engine.snapshot()).unwrap()
            );
        }
    }

    #[test]
    fn bounded_evidence_loss_is_explicit_not_whole_session_parity() {
        let mut evidence = RequestEvidence::default();
        for _ in 0..=RequestEvidence::MAX_OBSERVATIONS {
            evidence.reviewer_usage_settled(
                &json!({"input_tokens":1,"output_tokens":0}),
                true,
                Some(0.1),
            );
        }
        let estimate = evidence.snapshot();
        assert_eq!(
            estimate.coverage.prior_untracked_calls,
            RequestEvidence::MAX_OBSERVATIONS as u64
        );
        assert!(estimate.dashboard(false).whole_window.is_none());
        assert_eq!(evidence.observations.len(), 1);
    }
}
