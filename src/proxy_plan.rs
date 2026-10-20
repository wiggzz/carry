//! Plan the actual next primary and reviewer requests, not historical windows.
use super::{
    HashSet, ProxyCli, Session, Value, ViewCosts, cache_base, estimate, joint_decision, json, now,
    primary_fits, primary_rates, primary_required_tokens, primary_wire_body, resolved_view_costs,
    review_body, review_fits, reviewer_rates, reviewer_required_tokens, reviewer_reusable,
    reviewer_view_costs, select_removals, standard,
};

/// Frozen-source sensitivity: advance cadence and exposure, assuming future
/// reviews complete/merge before their next due dispatch. This is not a latency
/// guarantee. Already unfinished work is unknown, not free.
fn review_calls(
    config: &ProxyCli,
    session: &Session,
    body: &Value,
    changed: bool,
    allow_extra: bool,
) -> Option<Vec<Option<Value>>> {
    #[cfg(test)]
    super::planner_work::record("forecasts");
    if !standard(body) || primary_rates(config, body["model"].as_str().unwrap_or(""), 0.0).is_none()
    {
        return Some(Vec::new());
    }
    if session.background_review_pending
        || (session.bootstrap_review_pending && !session.bootstrap_review_complete)
    {
        return None;
    }
    let mut future = session.clone();
    super::admission::initialize(&mut future);
    let mut calls = Vec::new();
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
        let dispatch = request > 0 || (!changed && !session.bootstrap_review_pending);
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
            let targets = future.due_keep_targets();
            let targeted = if targets.is_empty() {
                None
            } else {
                let mut dispatch = future.clone();
                dispatch.keep_leases.as_mut().unwrap().pending =
                    Some(crate::proxy_state::PendingKeepReview {
                        request_id: "forecast".into(),
                        targets,
                    });
                Some(review_body(config, &dispatch))
            };
            // No outcome is known for a hypothetical targeted review. Reserve
            // its single extra attempt; do not invent repeated paid renewals.
            future.reserve_extra_keep_attempt();
            // The actual dispatcher skips an over-cap targeted body; do not
            // forecast a paid call that cannot be sent.
            if targeted
                .as_ref()
                .is_none_or(|wire| review_fits(config, wire))
            {
                calls.push(targeted);
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
    Some(calls)
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
    retained["input"] = json!(session.render_primary());
    plan_inner(config, session, &retained, Some(paid))
        .0
        .is_some_and(|candidate| candidate.render_primary() != session.render_primary())
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
    let wire = primary_wire_body(config, body);
    let reviewer = review_body(config, session);
    let capacity_override = capacity_body(config, session);
    let capacity = capacity_override.as_ref().unwrap_or(&reviewer);
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
    let keep_calls = keep_schedule.as_ref().map(|s| s.len() as u64);
    let keep_cost = keep_schedule
        .as_ref()
        .and_then(|s| review_cost(config, session, &reviewer, s));
    let mut reports = Vec::new();
    let mut best: Option<(Session, f64)> = None;
    // Cleaning only disposable reviewer turns is a real candidate. It can make
    // room without rewriting an otherwise valuable primary cache prefix.
    candidates.push(groups.iter().map(|g| g.id).collect());
    let mut evaluated = HashSet::new();
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
        let changed = candidate.render_primary() != session.render_primary();
        let mut compact = body.clone();
        compact["input"] = json!(candidate.render_primary());
        let compact_review = review_body(config, &candidate);
        let feasible = primary_fits(config, &compact);
        let compact = primary_wire_body(config, &compact);
        let compact_schedule =
            review_calls(config, &candidate, body, changed, acquisition.is_none());
        let compact_calls = compact_schedule.as_ref().map(|s| s.len() as u64);
        let shadow = keep_cost
            .zip(
                compact_schedule
                    .as_ref()
                    .and_then(|s| review_cost(config, &candidate, &compact_review, s)),
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
        let joint = primary
            .zip(shadow)
            .map(|(primary, shadow)| joint_decision(primary, shadow, config.min_payback_percent));
        // Keep the existing recurring-completion diagnostic intact. Selection
        // cannot spend hypothetical repeated background completions. Pending
        // paid work is sunk/unknown, not a reason to withhold valid primary advice.
        let due = !session.background_review_pending
            && !(session.bootstrap_review_pending && !session.bootstrap_review_complete)
            && reviewer_ok
            && super::admission::decision_for_plan(
                config,
                session,
                body,
                false,
                acquisition.is_none(),
            )["admitted"]
                == true;
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
                .and_then(|s| s.get(..1))
                .and_then(|s| review_cost(config, session, &reviewer, s))
                .zip(if changed {
                    // This rewrite skips the due review, but its next cold/write
                    // input is not free. Reserve ONE positive input delta only;
                    // do not invent a completion on the compacted branch.
                    rebuild_allowance
                } else {
                    compact_schedule
                        .as_ref()
                        .and_then(|s| s.get(..1))
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
        let selection_joint = primary
            .zip(selection_shadow)
            .map(|(primary, shadow)| joint_decision(primary, shadow, config.min_payback_percent));
        reports.push(json!({"removed_groups":removed,"feasible":feasible,"joint":joint,
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
            if best.as_ref().is_none_or(|(_, prior)| score < *prior) {
                best = Some((candidate, score));
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
    let report = json!({"selected":best.is_some(),"forced":forced,"reason":reason,
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
        "ordinary_review_admission":super::admission::decision_for_plan(config,session,body,false,acquisition.is_none()),
        "review_min_primary_tokens":config.review_min_primary_tokens,
        "review_min_new_tokens":config.review_min_new_tokens,
        "review_every_requests":config.review_every_requests,
        "review_backoff_factor":config.review_backoff_factor,"review_backoff_max":config.review_backoff_max,
        "estimate_basis":"frozen_source_and_timely_completion_sensitivity",
        "future_review_outcome_basis":"hold_current_backoff_no_invented_native_growth",
        "review_completion_basis":"future_reviews_complete_and_merge_before_next_due_dispatch",
        "cache_basis":"exact_compatible_prefix_receipts_expire_after_30_minutes",
        "candidates":reports});
    (best.map(|(state, _)| state), report)
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
            assert_eq!(schedule.len(), 2, "exposure and cadence must still advance");
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
