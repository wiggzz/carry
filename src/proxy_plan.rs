//! Plan the actual next primary and reviewer requests, not historical windows.
use super::{
    Digest, HashSet, ProxyCli, Session, Value, ViewCosts, cache_base, estimate, joint_decision,
    json, now, primary_fits, primary_rates, primary_required_tokens, primary_wire_body,
    resolved_view_costs, review_body, review_fits, reviewer_rates, reviewer_required_tokens,
    reviewer_reusable, reviewer_view_costs, select_removals, standard,
};

#[derive(Default)]
struct ReviewForecast {
    calls: Vec<Option<Value>>,
    immediate: bool,
}

/// Frozen-source sensitivity: advance cadence and exposure, assuming future
/// reviews complete/merge before their next due dispatch. This is not a latency
/// guarantee. Already unfinished work is unknown, not free.
fn review_calls(
    config: &ProxyCli,
    session: &Session,
    body: &Value,
    changed: bool,
    allow_extra: bool,
) -> Option<ReviewForecast> {
    #[cfg(test)]
    super::planner_work::record("forecasts");
    if !standard(body) || primary_rates(config, body["model"].as_str().unwrap_or(""), 0.0).is_none()
    {
        return Some(ReviewForecast::default());
    }
    if session.background_review_pending
        || (session.bootstrap_review_pending && !session.bootstrap_review_complete)
    {
        return None;
    }
    let mut future = session.clone();
    super::admission::initialize(&mut future);
    let mut forecast = ReviewForecast::default();
    // Only cadence, exposure, coverage and attempt reservation advance here;
    // history/removals/materialized memories and the primary wire policy do not.
    // Reuse just that immutable render/tokenization, never an admission decision
    // or economic result. A keyless Codex adapter generates a fresh wire key,
    // whose token count is not invariant, so preserve its per-decision projection.
    let reusable_primary = !config.codex_login || body.get("prompt_cache_key").is_some();
    let mut primary = None;
    for request in 0..config.payoff_requests {
        // A source rewrite skips this request's ordinary dispatch. Bootstrap
        // already paid for its source; no hypothetical native growth is added.
        let dispatch = request > 0
            || (!config.current_review_suppressed && !changed && !session.bootstrap_review_pending);
        let projected_primary = if dispatch && reusable_primary {
            Some(
                *primary
                    .get_or_insert_with(|| super::admission::primary_tokens(config, &future, body)),
            )
        } else {
            None
        };
        if dispatch
            && super::admission::decision_with_primary(
                config,
                &future,
                body,
                false,
                allow_extra,
                projected_primary,
            )["admitted"]
                == true
        {
            let targeted = if future.due_keep_targets().is_empty() {
                None
            } else {
                let mut dispatch = future.clone();
                let groups = dispatch.groups();
                Some(super::prepare_ordinary_review(
                    config,
                    &mut dispatch,
                    &groups,
                    "forecast",
                ))
            };
            // The actual dispatcher skips an over-cap targeted body; do not
            // forecast a paid call that cannot be sent.
            if targeted
                .as_ref()
                .is_none_or(|wire| review_fits(config, wire))
            {
                // A later forecast call is not evidence of immediate readiness.
                forecast.immediate |= request == 0;
                forecast.calls.push(targeted);
                // No outcome is known for a hypothetical targeted review.
                // Reserve only a dispatchable attempt; invent no paid renewals.
                future.reserve_extra_keep_attempt();
                future.last_review_request = future.completed_requests;
                let groups = future.groups();
                super::admission::cover(&mut future, &groups);
            }
        }
        future.completed_requests = future.completed_requests.saturating_add(1);
        for item in &mut future.history {
            if !item.removed {
                item.exposed = true;
            }
        }
    }
    Some(forecast)
}

fn review_cost(
    config: &ProxyCli,
    session: &Session,
    body: &Value,
    schedule: &[Option<Value>],
) -> Option<f64> {
    let calls = schedule.len() as u64;
    if calls == 0 {
        return Some(0.0);
    }
    if schedule.iter().any(Option::is_some) {
        // Lease annotations live in the volatile tail. Price each dispatched
        // body at its own tariff, preserving the existing stable-prefix warm
        // sensitivity for later calls, not discounting the new target ledger.
        let mut total = 0.0;
        for (index, targeted) in schedule.iter().enumerate() {
            let wire = targeted.as_ref().unwrap_or(body);
            let first = reviewer_view_costs(config, &session.reviewer_cache, wire, wire, 1)?.keep;
            let input = if index == 0 {
                first
            } else {
                reviewer_view_costs(config, &session.reviewer_cache, wire, wire, 2)?.keep - first
            };
            let rates = reviewer_rates(config, estimate(wire))?;
            total += input + config.classifier_max_output_tokens as f64 * rates.output;
        }
        return Some(total);
    }
    let inputs = reviewer_view_costs(config, &session.reviewer_cache, body, body, calls)?.keep;
    let rates = reviewer_rates(config, estimate(body))?;
    Some(inputs + calls as f64 * config.classifier_max_output_tokens as f64 * rates.output)
}

pub(super) fn plan(config: &ProxyCli, session: &Session, body: &Value) -> (Option<Session>, Value) {
    plan_inner(config, session, body, None)
}

pub(super) fn economic_candidate(
    config: &ProxyCli,
    session: &Session,
    body: &Value,
    paid: f64,
) -> bool {
    #[cfg(test)]
    super::planner_work::record("economic_probes");
    let mut retained = body.clone();
    #[cfg(test)]
    let primary_scope = super::planner_work::primary_scope("economic_primary_renders");
    let original_primary = session.render_primary();
    retained["input"] = json!(original_primary);
    #[cfg(test)]
    drop(primary_scope);
    plan_inner(config, session, &retained, Some(paid))
        .0
        .is_some_and(|candidate| {
            #[cfg(test)]
            let _primary_scope = super::planner_work::primary_scope("economic_primary_renders");
            candidate.render_primary() != original_primary
        })
}

fn capacity_body(config: &ProxyCli, session: &Session) -> Option<Value> {
    if session.background_review_pending
        && session
            .keep_leases
            .as_ref()
            .is_some_and(|s| s.pending.is_some())
    {
        // The worker still checks/sends its real targeted body. Its in-flight
        // tail annotations are not a new primary capacity dependency.
        let mut view = session.clone();
        view.keep_leases.as_mut().unwrap().pending = None;
        Some(review_body(config, &view))
    } else {
        None
    }
}

fn plan_inner(
    config: &ProxyCli,
    session: &Session,
    body: &Value,
    acquisition: Option<f64>,
) -> (Option<Session>, Value) {
    #[cfg(test)]
    super::planner_work::record("plans");
    // Direct planner callers and runtime dispatch share the same frozen epoch.
    let mut scoped = config.clone();
    scoped.model_resolution = session.model_resolution.clone();
    let config = &scoped;
    // An explicit request cap is an opt-in sensitivity, not expected generation.
    // Capacity headroom remains independent, including on subscription routes
    // that strip the cap from the adapted wire. Missing caps preserve legacy zero.
    let output_allowance = body["max_output_tokens"].as_u64();
    let output_tokens = output_allowance.unwrap_or(0) as f64 * config.payoff_requests as f64;
    let wire = primary_wire_body(config, body);
    let reviewer = review_body(config, session);
    let capacity_override = capacity_body(config, session);
    let capacity = capacity_override.as_ref().unwrap_or(&reviewer);
    let prediction = session.cost_trajectory.as_ref().and_then(|t| {
        t.receipt_prediction(
            &wire,
            &config.upstream_url,
            config.model_resolution.as_ref().map(|p| &p.primary),
        )
    });
    let reserve = body["max_output_tokens"]
        .as_u64()
        .unwrap_or(config.primary_output_reserve_tokens as u64);
    // A measured prefix plus estimated append is still a prediction. It may
    // strengthen, never lower, the original local-token capacity reservation.
    let predicted_required = primary_required_tokens(config, body).max(
        prediction.unwrap_or(0) as f64 * (1.0 + f64::from(config.context_margin_percent) / 100.0)
            + reserve as f64,
    );
    let primary_ok = primary_fits(config, body);
    let reviewer_ok = review_fits(config, capacity);
    let forced = acquisition.is_none() && !primary_ok;
    let groups = session.groups();
    let eligible = groups
        .iter()
        .filter(|g| session.drop_authorized(g))
        .map(|g| g.id)
        .collect::<HashSet<_>>();
    let mut candidates = vec![HashSet::new()];
    // Prefix receipts work for implicit caching too; explicit markers are not
    // required to consider preserving a previously read primary prefix.
    let mut rendered = session.render_primary_with_ids();
    let mut folded = HashSet::new();
    if config.codex_login {
        let leading = rendered
            .iter()
            .take_while(|(_, v)| matches!(v["role"].as_str(), Some("system" | "developer")))
            .count();
        folded.extend(rendered.drain(..leading).filter_map(|(id, _)| id));
    }
    for evidence in &session.primary_cache {
        if evidence.matches_route(config.model_resolution.as_ref().map(|p| &p.primary.route))
            && now().saturating_sub(evidence.at) <= 1800
            && wire["input"]
                .as_array()
                .is_some_and(|input| evidence.matches(&cache_base(&wire), input))
        {
            let ids = rendered
                .iter()
                .take(evidence.input_len())
                .filter_map(|(id, _)| *id)
                .chain(folded.iter().copied())
                .collect::<HashSet<_>>();
            candidates.push(
                groups
                    .iter()
                    .filter(|g| g.members.iter().any(|id| ids.contains(id)))
                    .map(|g| g.id)
                    .collect(),
            );
        }
    }
    let keep_schedule = if reviewer_ok {
        review_calls(config, session, body, false, acquisition.is_none())
    } else {
        None // deferred reviewer schedule; not primary infeasibility
    };
    let keep_calls = keep_schedule.as_ref().map(|s| s.calls.len() as u64);
    let keep_cost = keep_schedule
        .as_ref()
        .and_then(|s| review_cost(config, session, &reviewer, &s.calls));
    // One frozen admission decision is shared by all candidate comparisons and
    // the report, rather than repeatedly projecting the same primary per row.
    let ordinary_admission =
        super::admission::decision_for_plan(config, session, body, false, acquisition.is_none());
    let due = keep_schedule
        .as_ref()
        .is_some_and(|schedule| schedule.immediate);
    let mut reports = Vec::new();
    let mut best: Option<(Session, f64, String, &'static str)> = None;
    // Cleaning only disposable reviewer turns is a real candidate. It can make
    // room without rewriting an otherwise valuable primary cache prefix.
    candidates.push(groups.iter().map(|g| g.id).collect());
    let mut evaluated = HashSet::new();
    // Preserve the complete raw view, not the Codex-folded cache-slot view.
    // Render lazily so plans with no legal actions do no additional work.
    let mut original_primary = None;
    for protected in candidates {
        let removed = select_removals(groups.iter().map(|g| g.id), &eligible, &protected);
        if removed.is_empty()
            && session.review_history.is_none()
            && session.released_memory_ids().is_empty()
        {
            continue;
        }
        // Cache-frontier protections are not distinct actions when they yield
        // the same complete removals. State, memory retirement and forecasts
        // are identical; evaluate that expensive action once.
        if !evaluated.insert(removed.clone()) {
            continue;
        }
        let mut candidate = session.clone();
        candidate.remove(&removed, &groups);
        #[cfg(test)]
        let primary_scope = super::planner_work::primary_scope("candidate_primary_renders");
        let candidate_primary = candidate.render_primary();
        let changed =
            candidate_primary != *original_primary.get_or_insert_with(|| session.render_primary());
        let action = if changed { "joint" } else { "reviewer_only" };
        // Identity names the action, not its score, enumeration index, cap, or
        // generated transport keys. Memory retirement can change primary even
        // without source removal. Keep the legacy candidate vector unchanged.
        let mut retired_memories = session
            .released_memory_ids()
            .into_iter()
            .collect::<Vec<_>>();
        retired_memories.sort();
        let candidate_id = format!(
            "{:x}",
            super::Sha256::digest(
                serde_json::to_vec(&(action, &removed, &retired_memories)).unwrap()
            )
        );
        let mut compact = body.clone();
        compact["input"] = json!(candidate_primary);
        #[cfg(test)]
        drop(primary_scope);
        let compact_review = review_body(config, &candidate);
        let feasible = primary_fits(config, &compact);
        let compact = primary_wire_body(config, &compact);
        let compact_schedule =
            review_calls(config, &candidate, body, changed, acquisition.is_none());
        let compact_calls = compact_schedule.as_ref().map(|s| s.calls.len() as u64);
        let shadow = keep_cost
            .zip(
                compact_schedule
                    .as_ref()
                    .and_then(|s| review_cost(config, &candidate, &compact_review, &s.calls)),
            )
            .map(|(keep, compact)| ViewCosts {
                keep,
                compact: compact + acquisition.unwrap_or(0.0),
            });
        let primary = if standard(body) {
            resolved_view_costs(
                &wire,
                &compact,
                &session.primary_cache,
                wire["model"].as_str().unwrap_or(""),
                config.payoff_requests,
                config.model_resolution.as_ref().map(|pair| &pair.primary),
            )
        } else {
            None
        };
        let output = if primary.is_none() {
            None
        } else if output_tokens == 0.0 {
            Some(ViewCosts {
                keep: 0.0,
                compact: 0.0,
            })
        } else {
            primary_rates(
                config,
                wire["model"].as_str().unwrap_or(""),
                estimate(&wire),
            )
            .zip(primary_rates(
                config,
                compact["model"].as_str().unwrap_or(""),
                estimate(&compact),
            ))
            .map(|(keep, compact)| ViewCosts {
                keep: output_tokens * keep.output,
                compact: output_tokens * compact.output,
            })
        };
        let selection_primary = primary.zip(output).map(|(input, output)| ViewCosts {
            keep: input.keep + output.keep,
            compact: input.compact + output.compact,
        });
        let joint = primary
            .zip(shadow)
            .map(|(primary, shadow)| joint_decision(primary, shadow, config.min_payback_percent));
        // Keep the existing recurring-completion diagnostic intact. Selection
        // cannot spend hypothetical repeated background completions. Pending
        // paid work is sunk/unknown, not a reason to withhold valid primary advice.
        // A local replay rebuild is not a paid summarizer call. When no review
        // is due, reserve only its positive input cache/write delta under ONE
        // explicitly conditional frozen-source next-request scenario; never
        // credit savings or output completions from that unknown future work.
        // An oversized rendering is not a request we can dispatch or price as
        // one. Defer it rather than let fictional reviewer work veto primary.
        let rebuild_allowance = if review_fits(config, &compact_review) && reviewer_ok {
            reviewer_view_costs(
                config,
                &session.reviewer_cache,
                &reviewer,
                &compact_review,
                1,
            )
            .map(|cost| (cost.compact - cost.keep).max(0.0))
        } else {
            Some(0.0)
        };
        let selection_shadow = if due {
            keep_schedule
                .as_ref()
                .and_then(|s| s.calls.get(..1))
                .and_then(|s| review_cost(config, session, &reviewer, s))
                .zip(if changed {
                    // This rewrite skips the due review, but its next cold/write
                    // input is not free. Reserve ONE positive input delta only;
                    // do not invent a completion on the compacted branch.
                    rebuild_allowance
                } else {
                    compact_schedule
                        .as_ref()
                        .and_then(|s| s.calls.get(..1))
                        .and_then(|s| review_cost(config, &candidate, &compact_review, s))
                })
                .map(|(keep, compact)| ViewCosts { keep, compact })
        } else {
            rebuild_allowance.map(|allowance| ViewCosts {
                keep: 0.0,
                compact: allowance,
            })
        };
        let selection_shadow = selection_shadow.map(|cost| ViewCosts {
            keep: cost.keep,
            compact: cost.compact + acquisition.unwrap_or(0.0),
        });
        let selection_joint = selection_primary
            .zip(selection_shadow)
            .map(|(primary, shadow)| joint_decision(primary, shadow, config.min_payback_percent));
        reports.push(json!({"candidate_id":candidate_id,"action":action,
            "removed_groups":removed,"retired_memory_ids":retired_memories,"feasible":feasible,"joint":joint,
            "selection_primary_input":primary.map(|c| json!({"keep":c.keep,"compact":c.compact})),
            "selection_primary_output":output.map(|c| json!({"keep":c.keep,"compact":c.compact})),
            "selection_joint":selection_joint,
            "selection_review_basis":if due {"presently_due_next_review"} else {"conditional_one_request_input_rebuild_allowance_no_review_savings"},
            "selection_rebuild_allowance_assumption":"one_frozen_source_next_request_if_both_renderings_fit_not_a_dispatch_forecast",
            "selection_reviewer_output_completions":if due {Some(u64::from(!changed))} else {None},
            "selection_future_review_total_unknown":keep_calls.is_none(),
            "future_reviewer_keep_calls":keep_calls,"future_reviewer_compact_calls":compact_calls,
            "primary_retained_estimated_tokens":estimate(&compact),
            "reviewer_retained_estimated_tokens":estimate(&compact_review),
            "reviewer_cached_estimated_tokens":reviewer_reusable(config,&session.reviewer_cache,&compact_review)}));
        if feasible && (forced || selection_joint.is_some_and(|decision| decision.accepted)) {
            let score = if forced {
                selection_joint.map_or_else(|| estimate(&compact), |d| d.compact)
            } else {
                -selection_joint.map_or(0.0, |d| d.savings)
            };
            if best.as_ref().is_none_or(|(_, prior, _, _)| score < *prior) {
                best = Some((candidate, score, candidate_id, action));
            }
        }
    }
    let reason = if forced && best.is_none() {
        Some("context_capacity_exceeded")
    } else if !standard(body)
        || primary_rates(
            config,
            wire["model"].as_str().unwrap_or(""),
            estimate(&wire),
        )
        .is_none()
    {
        Some("unsupported_model_or_tier")
    } else if keep_calls.is_none() {
        Some("future_reviewer_schedule_unknown")
    } else {
        None
    };
    let identity_and_output = json!({
        "selected_candidate_id":best.as_ref().map(|(_, _, id, _)| id.as_str()).or(if forced {None} else {Some("keep")}),
        "selected_action":best.as_ref().map(|(_, _, _, action)| *action).or(if forced {None} else {Some("keep")}),
        "keep_candidate":{"candidate_id":"keep","action":"keep","feasible":primary_ok},
        "candidate_identity_basis":"action_ordered_removed_groups_sorted_retired_memory_ids_v1",
        "candidate_identity_scope":"actions_within_one_frozen_source_plan_not_global_session_identity",
        "primary_only_action":"not_applicable_source_removal_rebuilds_reviewer_replay",
        "primary_output_allowance_tokens":output_allowance.unwrap_or(0),
        "primary_output_allowance_requests":config.payoff_requests,
        "primary_output_allowance_basis":if output_allowance.is_some() {"request_max_output_tokens_cap_sensitivity_not_expected_generation"} else {"legacy_zero_no_explicit_request_cap_not_headroom"},
        "primary_output_tariff_basis":"each_rendered_branch_input_selected_frozen_tariff",
        "primary_receipt_prediction_tokens":prediction,
        "primary_receipt_predicted_reserved_tokens":prediction.map(|_| predicted_required),
        "primary_receipt_prediction_basis":"compatible_measured_prefix_plus_locally_estimated_appended_block_not_per_message_counts",
        "joint_basis":"legacy_input_only_primary_recurring_reviewer_sensitivity"});
    let mut report = json!({"selected":best.is_some(),"forced":forced,"reason":reason,
        "mode":format!("{:?}",config.mode).to_lowercase(),
        "primary_fits":primary_ok,"reviewer_fits":reviewer_ok,
        "primary_estimated_tokens":estimate(&wire),"reviewer_estimated_tokens":estimate(&reviewer),
        "primary_required_tokens":primary_required_tokens(config,body),
        "reviewer_required_tokens":reviewer_required_tokens(config,capacity),
        "reviewer_targeted_required_tokens":reviewer_required_tokens(config,&reviewer),
        "primary_context_tokens":config.primary_context_tokens,
        "reviewer_context_tokens":config.classifier_max_input_tokens,
        "context_margin_percent":config.context_margin_percent,
        "payoff_requests":config.payoff_requests,"min_payback_percent":config.min_payback_percent,
        "future_review_events":keep_calls,"future_reviewer_calls":keep_calls,
        "current_review_cost_is_sunk":session.shadow.calls > 0 &&
            (session.last_review_request == session.completed_requests ||
             (session.bootstrap_review_pending && session.bootstrap_review_complete)),
        "ordinary_review_admission":ordinary_admission,
        "current_review_dispatch_suppressed":config.current_review_suppressed,
        "review_min_primary_tokens":config.review_min_primary_tokens,
        "review_min_new_tokens":config.review_min_new_tokens,
        "review_every_requests":config.review_every_requests,
        "review_backoff_factor":config.review_backoff_factor,"review_backoff_max":config.review_backoff_max,
        "estimate_basis":"frozen_source_and_timely_completion_sensitivity",
        "future_review_outcome_basis":"hold_current_backoff_no_invented_native_growth",
        "review_completion_basis":"future_reviews_complete_and_merge_before_next_due_dispatch",
        "cache_basis":"exact_compatible_prefix_receipts_expire_after_30_minutes",
        "candidates":reports});
    report
        .as_object_mut()
        .unwrap()
        .extend(identity_and_output.as_object().unwrap().clone());
    (best.map(|(state, _, _, _)| state), report)
}

#[cfg(test)]
mod projection_controls {
    use super::*;
    use clap::Parser;

    #[test]
    fn frozen_primary_reuse_respects_generated_keys_exposure_and_pending_work() {
        let mut config = ProxyCli::try_parse_from([
            "carry proxy",
            "--review-min-primary-tokens",
            "0",
            "--review-min-new-tokens",
            "0",
            "--review-every-requests",
            "1",
            "--payoff-requests",
            "3",
        ])
        .unwrap();
        let mut session = Session::default();
        session
            .ingest(&[
                json!({"role":"user","content":"goal"}),
                json!({"role":"assistant","content":"evidence"}),
                json!({"role":"user","content":"continue"}),
            ])
            .unwrap();
        // Initially unexposed: the first projection must not admit a review;
        // later projections must recompute exposure/eligibility and cadence.
        for (codex, key, expected) in [(false, false, 1), (true, true, 1), (true, false, 3)] {
            config.codex_login = codex;
            let mut body = json!({"model":"gpt-6-sol","input":session.render_primary()});
            if key {
                body["prompt_cache_key"] = json!("stable-control-key");
            }
            super::super::planner_work::start();
            let schedule = review_calls(&config, &session, &body, false, false).unwrap();
            let counts = super::super::planner_work::finish();
            assert_eq!(
                schedule.calls.len(),
                2,
                "exposure and cadence must still advance"
            );
            assert!(!schedule.immediate, "later forecast calls are not due now");
            assert_eq!(counts["admission_primary_projections"], expected);

            let mut pending = session.clone();
            pending.background_review_pending = true;
            super::super::planner_work::start();
            assert!(review_calls(&config, &pending, &body, false, false).is_none());
            let counts = super::super::planner_work::finish();
            assert_eq!(counts.get("admission_primary_projections"), None);
        }
    }
}
