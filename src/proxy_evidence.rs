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
}
impl PrimaryFacts {
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
            fixed: needs_floor
                .then(|| crate::savings::fixed_prompt_tokens(body))
                .flatten(),
            protected: needs_floor
                .then(|| crate::savings::protected_prompt_tokens(body))
                .flatten(),
            cache_scope: crate::proxy_cache::modeled_scope(
                body,
                &model,
                (route, resolved.map(|r| &r.route)),
            ),
            cache_mode: request_cache_mode(body, &model, resolved),
            read_refresh: resolved
                .and_then(|r| r.scenario_cache_policy.as_ref())
                .and_then(|p| p.read_refresh_seconds(&model, body)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum Observation {
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
    Boundary,
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
    #[serde(skip)]
    restoration: Option<crate::proxy_quantity::RestorationEstimate>,
    #[serde(skip)]
    edited: bool,
}
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
            restoration: Some(crate::proxy_quantity::RestorationEstimate {
                tokens: 0,
                measured: true,
                estimated_opaque_items: 0,
            }),
            edited: false,
        }
    }
}
impl RequestEvidence {
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
            .filter(|observation| !matches!(observation, Observation::Boundary))
            .count() as u64;
        self.observations.clear();
    }
    fn append(&mut self, mut observation: Observation) {
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
        let mut replay = Replay {
            scenario: self.scenario,
            ..Replay::default()
        };
        replay.prior_untracked(self.prior_untracked);
        let (mut wire, mut semantic) = (Vec::new(), Vec::new());
        for observation in &self.observations {
            match observation {
                Observation::Primary {
                    facts,
                    restoration,
                    edited,
                } => {
                    let mut expanded = facts.as_ref().clone();
                    if Self::expand(facts, &mut wire, &mut semantic) {
                        expanded.input = Some(wire.clone());
                        expanded.semantic_input = Some(semantic.clone());
                    } else {
                        expanded.input = None;
                        expanded.semantic_input = None;
                    }
                    replay.projected_evidence(*restoration, *edited);
                    replay.observed_facts(&expanded);
                }
                Observation::Reviewer {
                    usage,
                    standard,
                    cost,
                } => replay.observed_reviewer(usage, *standard, *cost),
                Observation::FailedPrimary => replay.failed_call(),
                Observation::FailedReviewer => replay.failed_reviewer_call(),
                Observation::Unmodeled { cost } => replay.unmodeled_call(*cost),
                Observation::Boundary => replay.reset_epoch(),
            }
        }
        replay.snapshot()
    }
    pub fn prior_untracked(&mut self, calls: u64) {
        self.prior_untracked += calls;
    }
    pub fn failed_call(&mut self) {
        self.append(Observation::FailedPrimary);
    }
    pub fn failed_reviewer_call(&mut self) {
        self.append(Observation::FailedReviewer);
    }
    pub fn unmodeled_call(&mut self, cost: Option<f64>) {
        self.append(Observation::Unmodeled { cost });
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
                    serde_json::to_value(evidence.snapshot().dashboard(false)).unwrap(),
                    serde_json::to_value(engine.snapshot().dashboard(false)).unwrap()
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
