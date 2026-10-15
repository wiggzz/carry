//! Plan the actual next primary and reviewer requests, not historical windows.
use super::{
    HashSet, ProxyCli, Session, Value, cache_base, estimate, joint_decision, json, now,
    primary_fits, primary_required_tokens, primary_wire_body, review_body, review_fits,
    reviewer_required_tokens, reviewer_reusable, reviewer_view_costs, select_removals, standard,
    view_costs,
};

pub(super) fn plan(config: &ProxyCli, session: &Session, body: &Value) -> (Option<Session>, Value) {
    let wire = primary_wire_body(config, body);
    let reviewer = review_body(config, session);
    let primary_ok = primary_fits(config, &wire);
    let reviewer_ok = review_fits(config, &reviewer);
    let forced = !primary_ok || !reviewer_ok;
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
    let review_calls = config
        .payoff_requests
        .div_ceil(config.review_every_requests);
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
        let mut compact = body.clone();
        compact["input"] = json!(candidate.render_primary());
        let compact = primary_wire_body(config, &compact);
        let compact_review = review_body(config, &candidate);
        let feasible = primary_fits(config, &compact) && review_fits(config, &compact_review);
        let shadow = reviewer_view_costs(
            config,
            &session.reviewer_cache,
            &reviewer,
            &compact_review,
            review_calls,
        )
        .and_then(|mut cost| {
            let keep_rate =
                carry::core::Rates::for_model(&config.classifier_model, estimate(&reviewer))?;
            let compact_rate =
                carry::core::Rates::for_model(&config.classifier_model, estimate(&compact_review))?;
            let outputs = review_calls as f64 * config.classifier_max_output_tokens as f64;
            cost.keep += outputs * keep_rate.output;
            cost.compact += outputs * compact_rate.output;
            Some(cost)
        });
        let joint = if standard(body) {
            view_costs(
                &wire,
                &compact,
                &session.primary_cache,
                wire["model"].as_str().unwrap_or(""),
                config.payoff_requests,
            )
            .zip(shadow)
            .map(|(primary, shadow)| joint_decision(primary, shadow, config.min_payback_percent))
        } else {
            None
        };
        reports.push(json!({"removed_groups":removed,"feasible":feasible,"joint":joint,
            "primary_retained_estimated_tokens":estimate(&compact),
            "reviewer_retained_estimated_tokens":estimate(&compact_review),
            "reviewer_cached_estimated_tokens":reviewer_reusable(config,&session.reviewer_cache,&compact_review)}));
        if feasible && (forced || joint.is_some_and(|decision| decision.accepted)) {
            let score = if forced {
                joint.map_or_else(
                    || estimate(&compact) + estimate(&compact_review),
                    |d| d.compact,
                )
            } else {
                -joint.map_or(0.0, |d| d.savings)
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
    } else {
        None
    };
    let report = json!({"selected":best.is_some(),"forced":forced,"reason":reason,
        "mode":format!("{:?}",config.mode).to_lowercase(),
        "primary_fits":primary_ok,"reviewer_fits":reviewer_ok,
        "primary_estimated_tokens":estimate(&wire),"reviewer_estimated_tokens":estimate(&reviewer),
        "primary_required_tokens":primary_required_tokens(config,&wire),
        "reviewer_required_tokens":reviewer_required_tokens(config,&reviewer),
        "primary_context_tokens":config.primary_context_tokens,
        "reviewer_context_tokens":config.classifier_max_input_tokens,
        "context_margin_percent":config.context_margin_percent,
        "payoff_requests":config.payoff_requests,"min_payback_percent":config.min_payback_percent,
        "future_review_events":review_calls,"future_reviewer_calls":review_calls,
        "current_review_cost_is_sunk":false,
        "estimate_basis":"next_rendered_requests_with_margin_and_output_reserve",
        "cache_basis":"exact_compatible_prefix_receipts_expire_after_30_minutes",
        "candidates":reports});
    (best.map(|(state, _)| state), report)
}
