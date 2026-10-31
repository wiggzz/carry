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
    let groups = session.groups();
    let eligible = groups
        .iter()
        .filter(|g| session.drop_authorized(g))
        .collect::<Vec<_>>();
    // The last dispatch identifies an alternative, never removal authority.
    // Revalidate whole CURRENT cohorts, restoring revoked/protected members and
    // retaining everything appended since that dispatch. Render current facts
    // at their existing anchors rather than remembering another copy of input.
    let submitted = session
        .last_submitted_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let frontier = submitted.iter().copied().max().unwrap_or(0);
    let omitted = eligible
        .iter()
        .filter(|g| {
            g.members
                .iter()
                .all(|id| *id <= frontier && !submitted.contains(id))
        })
        .flat_map(|g| g.members.iter().copied())
        .collect::<HashSet<_>>();
    let continuation = Projection {
        input: session.render_projection(&omitted),
        omitted,
    };
    let mut retained = body.clone();
    retained["input"] = json!(continuation.input);
    let mut original = body.clone();
    original["input"] = json!(canonical.input);
    // Canonical replay is still an alternative (including genuine rewarming).
    // An oversized canonical prompt does not force a new rewrite when the
    // authorized continuation fits. Conversely, do not default to an oversized
    // memory projection when the originals themselves fit.
    let baseline = if !primary_fits(config, &retained) && primary_fits(config, &original) {
        retained = original.clone();
        &canonical
    } else {
        &continuation
    };
    let wire = primary_wire_body(config, &retained);
    let canonical_wire = primary_wire_body(config, &original);
    let forced = !primary_fits(config, &retained);
    let prediction = session.cost_trajectory.as_ref().and_then(|t| {
        t.receipt_prediction(
            &wire,
            &config.upstream_url,
            config.model_resolution.as_ref().map(|p| &p.primary),
        )
    });
    let mut protected_options = vec![HashSet::new()];
    // Preserve each genuinely compatible canonical warm boundary as another
    // alternative, alongside the reauthorized previous projection below.
    for entry in &session.upstream_cache {
        if entry.matches(
            &canonical_wire,
            config.model_resolution.as_ref().map(|p| &p.primary),
            &config.primary_cache_auth,
            now(),
        ) {
            // The subscription adapter moves leading native direction items
            // into instructions. Include those base-bound members plus the
            // actual wire prefix, not the same count of canonical positions.
            let leading = canonical.input.len() - canonical_wire["input"].as_array().unwrap().len();
            let ids = session
                .history
                .iter()
                .take(leading + entry.input_len)
                .map(|i| i.id)
                .collect::<HashSet<_>>();
            protected_options.push(ids);
        }
    }
    let mut options = protected_options
        .into_iter()
        .map(|protected| {
            eligible
                .iter()
                .filter(|g| !g.members.iter().any(|id| protected.contains(id)))
                .flat_map(|g| g.members.iter().copied())
                .collect::<HashSet<_>>()
        })
        .collect::<Vec<_>>();
    if !baseline.omitted.is_empty() {
        options.push(baseline.omitted.clone());
        options.push(HashSet::new());
    }
    let review = review_body_projected(config, session, baseline);
    let due = admission::scheduled(config, session)["admitted"] == true;
    let mut best: Option<(Projection, f64, String)> = None;
    let mut reports = Vec::new();
    let mut seen = HashSet::new();
    for omitted in options {
        let is_baseline = omitted == baseline.omitted;
        let mut omitted = omitted.into_iter().collect::<Vec<_>>();
        omitted.sort_unstable();
        if (omitted.is_empty() && baseline.omitted.is_empty()) || !seen.insert(omitted.clone()) {
            continue;
        }
        let omitted = omitted.into_iter().collect::<HashSet<_>>();
        let input = session.render_projection(&omitted);
        let projection = Projection { omitted, input };
        let mut compact = body.clone();
        compact["input"] = json!(projection.input);
        let required = primary_required_tokens(config, &compact);
        let feasible = required <= config.primary_context_tokens as f64;
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
        let review_cost = |view: &Value| {
            if !due || !review_fits(config, view) {
                return Some(0.0);
            }
            let input = resolved_view_costs(
                view,
                view,
                &session.upstream_cache,
                &config.classifier_model,
                1,
                config.model_resolution.as_ref().map(|p| &p.reviewer),
                &config.reviewer_cache_auth,
            )?;
            let rates = reviewer_rates(config, estimate(view))?;
            Some(input.keep + config.classifier_max_output_tokens as f64 * rates.output)
        };
        let shadow = review_cost(&review)
            .zip(review_cost(&compact_review))
            .map(|(keep, compact)| ViewCosts { keep, compact });
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
        let id = if members.is_empty() {
            "keep".into()
        } else {
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&members).unwrap())
            )
        };
        reports.push(json!({"candidate_id":id,"action":if members.is_empty() {"keep"} else {"projection"},"baseline":is_baseline,"removed_groups":eligible.iter().filter(|g| g.members.iter().all(|m| projection.omitted.contains(m))).map(|g|g.id).collect::<Vec<_>>(),"omitted_members":members,"feasible":feasible,"selection_joint":joint,"primary_retained_estimated_tokens":estimate(&compact),"primary_retained_required_tokens":required}));
        if feasible && (is_baseline || forced || joint.is_some_and(|j| j.accepted)) {
            let score = if forced {
                estimate(&compact)
            } else if is_baseline {
                0.0
            } else {
                -joint.unwrap().savings
            };
            if best.as_ref().is_none_or(|(_, prior, _)| score < *prior) {
                best = Some((projection, score, id));
            }
        }
    }
    let selected = best.as_ref().is_some_and(|(p, _, _)| !p.omitted.is_empty());
    let mut report = json!({"selected":selected,"forced":forced,"primary_fits":!forced,
        "primary_cached_estimated_tokens":crate::proxy_cache::reusable(&session.upstream_cache,&wire,config.model_resolution.as_ref().map(|p|&p.primary),&config.primary_cache_auth,now()),
        "reviewer_fits":review_fits(config,&review),"primary_estimated_tokens":estimate(&wire),
        "primary_required_tokens":primary_required_tokens(config,&retained),"primary_receipt_prediction_tokens":prediction,
        "context_margin_percent":config.context_margin_percent,
        "primary_output_reserve_tokens":primary_output_reserve(config,&retained),
        "primary_output_reserve_basis":if retained["max_output_tokens"].as_u64().is_some() {"request_max_output_tokens"} else {"configured_capacity_reserve"},
        "reviewer_estimated_tokens":estimate(&review),"primary_context_tokens":config.primary_context_tokens,
        "selected_candidate_id":best.as_ref().map(|(_,_,id)|id.as_str()).or(if forced {None} else {Some("keep")}),
        "selected_action":if selected {"projection"} else {"keep"},
        "payoff_requests":config.payoff_requests,"min_payback_percent":config.min_payback_percent,
        "cache_basis":"actual_compatible_wire_terminal_receipts_30_minute_ttl",
        "estimate_basis":"frozen_primary_horizon_and_actual_current_review_schedule",
        "candidates":reports});
    if primary_rates(
        config,
        wire["model"].as_str().unwrap_or(""),
        estimate(&wire),
    )
    .is_none()
    {
        report["reason"] = json!("unsupported_model_or_tier");
    }
    report["reviewer_context_tokens"] = json!(
        config
            .model_resolution
            .as_ref()
            .map_or(Some(config.classifier_max_input_tokens), |p| p
                .reviewer
                .capacity_tokens)
    );
    report["reviewer_ordinary_context_tokens"] = json!(
        config
            .model_resolution
            .as_ref()
            .map(|p| p.reviewer.ordinary.total_budget)
    );
    report["primary_ordinary_context_tokens"] = json!(
        config
            .model_resolution
            .as_ref()
            .map(|p| p.primary.ordinary.total_budget)
    );
    report["model_resolution"] = json!(config.model_resolution);
    let mut baseline_members = baseline.omitted.iter().copied().collect::<Vec<_>>();
    baseline_members.sort_unstable();
    report["baseline_omitted_members"] = json!(baseline_members);
    report["canonical_primary_required_tokens"] = json!(primary_required_tokens(config, &original));
    (
        best.map(|(projection, _, _)| projection)
            .filter(|p| !p.omitted.is_empty()),
        report,
    )
}

/// One genuine no-fit acquisition, never an ordinary primary wait. The
/// optimistic fit is only an admission test; it grants no removal authority.
pub(super) fn acquisition_plausible(config: &ProxyCli, session: &Session, body: &Value) -> bool {
    if primary_fits(config, body)
        || session.background_review_pending
        || session.completed_review_len >= session.history.len()
    {
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
