//! Synchronous request-time economics. No selected state is ever persisted.
use super::*;

#[derive(Clone, Debug)]
pub(super) struct Projection {
    pub omitted: HashSet<u64>,
    pub input: Vec<Value>,
}
impl Projection {
    pub fn canonical(session: &Session) -> Self {
        Self {
            omitted: HashSet::new(),
            input: session.render_primary(),
        }
    }
}

pub(super) fn plan(
    config: &ProxyCli,
    session: &Session,
    body: &Value,
) -> (Option<Projection>, Value) {
    let canonical = Projection::canonical(session);
    let mut retained = body.clone();
    retained["input"] = json!(canonical.input);
    let wire = primary_wire_body(config, &retained);
    let forced = !primary_fits(config, &retained);
    let groups = session.groups();
    let eligible = groups
        .iter()
        .filter(|g| session.drop_authorized(g))
        .collect::<Vec<_>>();
    let mut options = vec![HashSet::new()];
    // Preserve each genuinely compatible warm boundary as a candidate. A past
    // outgoing omission has no role here: all candidates start from canonical H.
    for entry in &session.upstream_cache {
        if entry.matches(
            &wire,
            config.model_resolution.as_ref().map(|p| &p.primary),
            &config.primary_cache_auth,
            now(),
        ) {
            let ids = session
                .history
                .iter()
                .take(entry.input_len)
                .map(|i| i.id)
                .collect::<HashSet<_>>();
            options.push(ids);
        }
    }
    let review = review_body(config, session);
    let due = !session.background_review_pending
        && (session.bootstrap_review_pending
            || admission::decision(config, session, body, false)["admitted"] == true);
    let mut best: Option<(Projection, f64, String)> = None;
    let mut reports = Vec::new();
    let mut seen = HashSet::new();
    for protected in options {
        let mut omitted = eligible
            .iter()
            .filter(|g| !g.members.iter().any(|id| protected.contains(id)))
            .flat_map(|g| g.members.iter().copied())
            .collect::<Vec<_>>();
        omitted.sort_unstable();
        if omitted.is_empty() || !seen.insert(omitted.clone()) {
            continue;
        }
        let omitted = omitted.into_iter().collect::<HashSet<_>>();
        let input = session.render_projection(&omitted);
        let projection = Projection { omitted, input };
        let mut compact = body.clone();
        compact["input"] = json!(projection.input);
        let feasible = primary_fits(config, &compact);
        let compact = primary_wire_body(config, &compact);
        let primary = resolved_view_costs(
            &wire,
            &compact,
            &session.upstream_cache,
            wire["model"].as_str().unwrap_or(""),
            config.payoff_requests,
            config.model_resolution.as_ref().map(|p| &p.primary),
            &config.primary_cache_auth,
        );
        let compact_review = review_body_projected(config, session, &projection);
        // Same actual scheduling on both views: a selected rewrite does not
        // suppress the review. No hypothetical repeated reviewer savings.
        let shadow = if due && review_fits(config, &review) && review_fits(config, &compact_review)
        {
            resolved_view_costs(
                &review,
                &compact_review,
                &session.upstream_cache,
                &config.classifier_model,
                1,
                config.model_resolution.as_ref().map(|p| &p.reviewer),
                &config.reviewer_cache_auth,
            )
        } else {
            Some(ViewCosts {
                keep: 0.0,
                compact: 0.0,
            })
        };
        let output =
            body["max_output_tokens"].as_u64().unwrap_or(0) as f64 * config.payoff_requests as f64;
        let primary = primary.and_then(|cost| {
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
            .map(|(keep, drop)| ViewCosts {
                keep: cost.keep + output * keep.output,
                compact: cost.compact + output * drop.output,
            })
        });
        let joint = primary
            .zip(shadow)
            .map(|(p, s)| joint_decision(p, s, config.min_payback_percent));
        let mut members = projection.omitted.iter().copied().collect::<Vec<_>>();
        members.sort_unstable();
        let id = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&members).unwrap())
        );
        reports.push(json!({"candidate_id":id,"action":"projection","removed_groups":eligible.iter().filter(|g| g.members.iter().all(|m| projection.omitted.contains(m))).map(|g|g.id).collect::<Vec<_>>(),"omitted_members":members,"feasible":feasible,"selection_joint":joint,"primary_retained_estimated_tokens":estimate(&compact)}));
        if feasible && (forced || joint.is_some_and(|j| j.accepted)) {
            let score = if forced {
                estimate(&compact)
            } else {
                -joint.unwrap().savings
            };
            if best.as_ref().is_none_or(|(_, prior, _)| score < *prior) {
                best = Some((projection, score, id));
            }
        }
    }
    let report = json!({"selected":best.is_some(),"forced":forced,"primary_fits":!forced,
        "primary_cached_estimated_tokens":crate::proxy_cache::reusable(&session.upstream_cache,&wire,config.model_resolution.as_ref().map(|p|&p.primary),&config.primary_cache_auth,now()),
        "reviewer_fits":review_fits(config,&review),"primary_estimated_tokens":estimate(&wire),
        "primary_required_tokens":primary_required_tokens(config,&retained),
        "reviewer_estimated_tokens":estimate(&review),"primary_context_tokens":config.primary_context_tokens,
        "selected_candidate_id":best.as_ref().map(|(_,_,id)|id.as_str()).or(if forced {None} else {Some("keep")}),
        "selected_action":if best.is_some() {"projection"} else {"keep"},
        "payoff_requests":config.payoff_requests,"min_payback_percent":config.min_payback_percent,
        "cache_basis":"actual_compatible_wire_terminal_receipts_30_minute_ttl",
        "estimate_basis":"frozen_primary_horizon_and_actual_current_review_schedule",
        "candidates":reports});
    (best.map(|(projection, _, _)| projection), report)
}

/// One genuine no-fit acquisition, never an ordinary primary wait. The
/// optimistic fit is only an admission test; it grants no removal authority.
pub(super) fn acquisition_plausible(config: &ProxyCli, session: &Session, body: &Value) -> bool {
    if primary_fits(config, body) || session.background_review_pending {
        return false;
    }
    let omitted = session
        .groups()
        .iter()
        .filter(|g| {
            g.exposed
                && g.replayable
                && !g.pinned
                && !g.human
                && !g
                    .members
                    .iter()
                    .any(|id| session.opinions.get(id).is_some_and(|s| s == "keep"))
        })
        .flat_map(|g| g.members.iter().copied())
        .collect::<HashSet<_>>();
    let mut optimistic = body.clone();
    optimistic["input"] = json!(session.render_projection(&omitted));
    !omitted.is_empty()
        && primary_fits(config, &optimistic)
        && review_fits(config, &review_body(config, session))
        && reviewer_rates(config, estimate(&review_body(config, session))).is_some()
}
