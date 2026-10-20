//! Plan the actual next primary and reviewer requests, not historical windows.
use super::{
    HashSet, ProxyCli, Session, Value, ViewCosts, cache_base, estimate, joint_decision, json, now,
    primary_fits, primary_required_tokens, primary_wire_body, review_body, review_fits,
    reviewer_required_tokens, reviewer_reusable, reviewer_view_costs, select_removals, standard,
    view_costs,
};

/// Frozen-source sensitivity: advance cadence and exposure, assuming future
/// reviews complete/merge before their next due dispatch. This is not a latency
/// guarantee. Already unfinished work is unknown, not free.
fn review_calls(config: &ProxyCli, session: &Session, body: &Value, changed: bool) -> Option<u64> {
    if !standard(body)
        || carry::core::Rates::for_model(body["model"].as_str().unwrap_or(""), 0.0).is_none()
    {
        return Some(0);
    }
    if session.background_review_pending
        || (session.bootstrap_review_pending && !session.bootstrap_review_complete)
    {
        return None;
    }
    let mut future = session.clone();
    super::admission::initialize(&mut future);
    let mut calls = 0;
    for request in 0..config.payoff_requests {
        // A source rewrite skips this request's ordinary dispatch. Bootstrap
        // already paid for its source; no hypothetical native growth is added.
        let dispatch = request > 0 || (!changed && !session.bootstrap_review_pending);
        if dispatch && super::admission::decision(config, &future, body, false)["admitted"] == true
        {
            calls += 1;
            future.last_review_request = future.completed_requests;
            let groups = future.groups();
            super::admission::cover(&mut future, &groups);
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

fn review_cost(config: &ProxyCli, session: &Session, body: &Value, calls: u64) -> Option<f64> {
    if calls == 0 {
        return Some(0.0);
    }
    let inputs = reviewer_view_costs(config, &session.reviewer_cache, body, body, calls)?.keep;
    let rates = carry::core::Rates::for_model(&config.classifier_model, estimate(body))?;
    Some(inputs + calls as f64 * config.classifier_max_output_tokens as f64 * rates.output)
}

pub(super) fn plan(config: &ProxyCli, session: &Session, body: &Value) -> (Option<Session>, Value) {
    let wire = primary_wire_body(config, body);
    let reviewer = review_body(config, session);
    let primary_ok = primary_fits(config, body);
    let reviewer_ok = review_fits(config, &reviewer);
    let forced = !primary_ok;
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
        if now().saturating_sub(evidence.at) <= 1800
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
    let keep_calls = if reviewer_ok {
        review_calls(config, session, body, false)
    } else {
        None // deferred reviewer schedule; not primary infeasibility
    };
    let keep_cost = keep_calls.and_then(|calls| review_cost(config, session, &reviewer, calls));
    let mut reports = Vec::new();
    let mut best: Option<(Session, f64)> = None;
    // Cleaning only disposable reviewer turns is a real candidate. It can make
    // room without rewriting an otherwise valuable primary cache prefix.
    candidates.push(groups.iter().map(|g| g.id).collect());
    for protected in candidates {
        let removed = select_removals(groups.iter().map(|g| g.id), &eligible, &protected);
        if removed.is_empty() && session.review_history.is_none() {
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
        let compact_calls = review_calls(config, &candidate, body, changed);
        let shadow = keep_cost
            .zip(
                compact_calls
                    .and_then(|calls| review_cost(config, &candidate, &compact_review, calls)),
            )
            .map(|(keep, compact)| ViewCosts { keep, compact });
        let primary = if standard(body) {
            view_costs(
                &wire,
                &compact,
                &session.primary_cache,
                wire["model"].as_str().unwrap_or(""),
                config.payoff_requests,
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
            && super::admission::decision(config, session, body, false)["admitted"] == true;
        let incremental = review_cost(config, session, &reviewer, 1)
            .zip(review_cost(config, &candidate, &compact_review, 1))
            .map(|(keep, compact)| ViewCosts {
                keep: 0.0,
                compact: (compact - keep).max(0.0),
            });
        let selection_shadow = if !config.background_reviews && shadow.is_some() {
            shadow
        } else if due {
            review_cost(config, session, &reviewer, 1)
                .zip(review_cost(
                    config,
                    &candidate,
                    &compact_review,
                    u64::from(!changed),
                ))
                .map(|(keep, compact)| ViewCosts { keep, compact })
        } else {
            // A frozen-source one-rebuild debit, not zero-priced future work.
            // Credit no recurring savings, and charge any cold/cache/tier loss
            // on the rendered replay even while its dispatch time is unknown.
            incremental
        };
        let selection_joint = primary
            .zip(selection_shadow)
            .map(|(primary, shadow)| joint_decision(primary, shadow, config.min_payback_percent));
        reports.push(json!({"removed_groups":removed,"feasible":feasible,"joint":joint,
            "selection_joint":selection_joint,
            "selection_review_basis":if !config.background_reviews && shadow.is_some() {"recurring_completion_sensitivity"} else if due {"presently_due_next_review"} else {"incremental_one_rebuild_debit_no_review_savings"},
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
        || carry::core::Rates::for_model(wire["model"].as_str().unwrap_or(""), estimate(&wire))
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
        "reviewer_required_tokens":reviewer_required_tokens(config,&reviewer),
        "primary_context_tokens":config.primary_context_tokens,
        "reviewer_context_tokens":config.classifier_max_input_tokens,
        "context_margin_percent":config.context_margin_percent,
        "payoff_requests":config.payoff_requests,"min_payback_percent":config.min_payback_percent,
        "future_review_events":keep_calls,"future_reviewer_calls":keep_calls,
        "current_review_cost_is_sunk":session.shadow.calls > 0 &&
            (session.last_review_request == session.completed_requests ||
             (session.bootstrap_review_pending && session.bootstrap_review_complete)),
        "ordinary_review_admission":super::admission::decision(config,session,body,false),
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
