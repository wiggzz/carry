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
    // One decision, one eligibility instant: candidate enumeration, both lanes
    // of every comparison and the report cannot straddle different deadlines.
    let at = now();
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
    // These immutable views do not change while candidates are evaluated.
    // Reuse their capacity measurements instead of tokenizing the same large
    // input again for baseline selection, forced status and report fields.
    let resolved = config.model_resolution.as_ref().map(|p| &p.primary);
    let continuation_wire = primary_wire_body(config, &retained);
    let canonical_wire = if retained == original {
        continuation_wire.clone()
    } else {
        primary_wire_body(config, &original)
    };
    let continuation_input =
        session
            .primary_input
            .estimate(&continuation_wire, &config.upstream_url, resolved);
    let continuation_required =
        continuation_input.map(|input| primary_required_input(config, &retained, input.tokens));
    let canonical_input = if retained == original {
        continuation_input
    } else {
        session.primary_input.projected_estimate(
            &canonical_wire,
            &continuation_wire,
            &config.upstream_url,
            resolved,
        )
    };
    let canonical_required =
        canonical_input.map(|input| primary_required_input(config, &original, input.tokens));
    let continuation_fits = continuation_required
        .is_none_or(|required| required <= config.primary_context_tokens as f64);
    let (baseline, baseline_required, baseline_input, wire) = if !continuation_fits
        && canonical_required
            .is_some_and(|required| required <= config.primary_context_tokens as f64)
    {
        retained = original.clone();
        (
            &canonical,
            canonical_required,
            canonical_input,
            canonical_wire.clone(),
        )
    } else {
        (
            &continuation,
            continuation_required,
            continuation_input,
            continuation_wire,
        )
    };
    let wire_tokens = baseline_input.map(|input| input.tokens);
    let baseline_fits =
        baseline_required.map(|required| required <= config.primary_context_tokens as f64);
    let forced = baseline_fits == Some(false);
    let prediction = session
        .primary_input
        .known_estimate(&wire, &config.upstream_url, resolved)
        .filter(|input| input.basis != crate::proxy_quantity::EstimateBasis::LocalVisibleText)
        .map(|input| input.tokens);
    let mut protected_options = vec![HashSet::new()];
    // Preserve each genuinely compatible canonical warm boundary as another
    // alternative, alongside the reauthorized previous projection below.
    for entry in &session.upstream_cache {
        if entry
            .reusable_at(
                &canonical_wire,
                config.model_resolution.as_ref().map(|p| &p.primary),
                &config.primary_cache_auth,
                at,
            )
            .is_some()
        {
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
    let due = admission::scheduled(config, session)["admitted"] == true;
    let review = due.then(|| review_body_projected(config, session, baseline));
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
        let compact = primary_wire_body(config, &compact);
        let compact_input = session.primary_input.projected_estimate(
            &compact,
            &wire,
            &config.upstream_url,
            resolved,
        );
        let required =
            compact_input.map(|input| primary_required_input(config, &compact, input.tokens));
        let feasible =
            required.is_none_or(|required| required <= config.primary_context_tokens as f64);
        let primary = baseline_input
            .zip(compact_input)
            .and_then(|(keep_input, drop_input)| {
                resolved_input_costs(
                    &wire,
                    &compact,
                    &session.upstream_cache,
                    wire["model"].as_str().unwrap_or(""),
                    config.payoff_requests,
                    config.model_resolution.as_ref().map(|p| &p.primary),
                    &config.primary_cache_auth,
                    at,
                    keep_input,
                    drop_input,
                )
            });
        let compact_review = due.then(|| review_body_projected(config, session, &projection));
        // Same actual scheduling on both views: a selected rewrite does not
        // suppress the review. No hypothetical repeated reviewer savings.
        let review_cost = |view: Option<&Value>| {
            let Some(view) = view else {
                return Some(0.0);
            };
            if !review_fits_measured(config, session, view) {
                return Some(0.0);
            }
            let quantity = session.reviewer_input.estimate(
                view,
                config
                    .classifier_url
                    .as_deref()
                    .unwrap_or(&config.upstream_url),
                config.model_resolution.as_ref().map(|p| &p.reviewer),
            )?;
            let input = resolved_input_costs(
                view,
                view,
                &session.upstream_cache,
                &config.classifier_model,
                1,
                config.model_resolution.as_ref().map(|p| &p.reviewer),
                &config.reviewer_cache_auth,
                at,
                quantity,
                quantity,
            )?;
            let rates = reviewer_rates(config, quantity.tokens as f64)?;
            Some(input.keep + config.classifier_max_output_tokens as f64 * rates.output)
        };
        let shadow = review_cost(review.as_ref())
            .zip(review_cost(compact_review.as_ref()))
            .map(|(keep, compact)| ViewCosts { keep, compact });
        let output =
            body["max_output_tokens"].as_u64().unwrap_or(0) as f64 * config.payoff_requests as f64;
        let primary = primary.and_then(|cost| {
            primary_rates(
                config,
                wire["model"].as_str().unwrap_or(""),
                wire_tokens? as f64,
            )
            .zip(primary_rates(
                config,
                compact["model"].as_str().unwrap_or(""),
                compact_input?.tokens as f64,
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
        reports.push(json!({"candidate_id":id,"action":if members.is_empty() {"keep"} else {"projection"},"baseline":is_baseline,"removed_groups":eligible.iter().filter(|g| g.members.iter().all(|m| projection.omitted.contains(m))).map(|g|g.id).collect::<Vec<_>>(),"omitted_members":members,"feasible":feasible,"selection_joint":joint,"primary_retained_estimated_tokens":compact_input.map(|input|input.tokens),"primary_input_basis":compact_input.map(|input|input.basis),"primary_retained_required_tokens":required}));
        if feasible && (is_baseline || forced || joint.is_some_and(|j| j.accepted)) {
            let score = if forced {
                compact_input.map_or(f64::INFINITY, |input| input.tokens as f64)
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
    let mut report = json!({"selected":selected,"forced":forced,"primary_fits":baseline_fits,
        "primary_cached_estimated_tokens":crate::proxy_cache::reusable(&session.upstream_cache,&wire,config.model_resolution.as_ref().map(|p|&p.primary),&config.primary_cache_auth,at),
        "reviewer_fits":review.as_ref().map(|review|review_fits_measured(config,session,review)),"primary_estimated_tokens":wire_tokens,
        "primary_required_tokens":baseline_required,"primary_receipt_prediction_tokens":prediction,
        "primary_input_basis":baseline_input.map(|input|input.basis),"primary_admission":"provider_enforced",
        "context_margin_percent":config.context_margin_percent,
        "primary_output_reserve_tokens":primary_output_reserve(config,&retained),
        "primary_output_reserve_basis":if retained["max_output_tokens"].as_u64().is_some() {"request_max_output_tokens"} else {"configured_capacity_reserve"},
        "reviewer_estimated_tokens":review.as_ref().and_then(|review|session.reviewer_input.known_estimate(review,
            config.classifier_url.as_deref().unwrap_or(&config.upstream_url),
            config.model_resolution.as_ref().map(|p|&p.reviewer))).map(|input|input.tokens),"primary_context_tokens":config.primary_context_tokens,
        "selected_candidate_id":best.as_ref().map(|(_,_,id)|id.as_str()).or(if forced {None} else {Some("keep")}),
        "selected_action":if selected {"projection"} else {"keep"},
        "payoff_requests":config.payoff_requests,"min_payback_percent":config.min_payback_percent,
        "cache_basis":"actual_compatible_wire_terminal_receipts_30_minute_ttl",
        "estimate_basis":"frozen_primary_horizon_and_actual_current_review_schedule",
        "candidates":reports});
    if wire_tokens.is_none() {
        report["reason"] = json!("input_quantity_unknown");
    } else if wire_tokens
        .and_then(|input| primary_rates(config, wire["model"].as_str().unwrap_or(""), input as f64))
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
    report["canonical_primary_required_tokens"] = json!(canonical_required);
    (
        best.map(|(projection, _, _)| projection)
            .filter(|p| !p.omitted.is_empty()),
        report,
    )
}

/// One genuine no-fit acquisition, never an ordinary primary wait. The
/// optimistic fit is only an admission test; it grants no removal authority.
pub(super) fn acquisition_plausible(config: &ProxyCli, session: &Session, body: &Value) -> bool {
    let wire = primary_wire_body(config, body);
    let resolved = config.model_resolution.as_ref().map(|p| &p.primary);
    let source = session
        .primary_input
        .estimate(&wire, &config.upstream_url, resolved);
    if source.is_none_or(|input| {
        primary_required_input(config, body, input.tokens) <= config.primary_context_tokens as f64
    }) || session.background_review_pending
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
    if omitted.is_empty() {
        return false;
    }
    let optimistic_wire = primary_wire_body(config, &optimistic);
    let fits = session
        .primary_input
        .projected_estimate(&optimistic_wire, &wire, &config.upstream_url, resolved)
        .is_some_and(|input| {
            primary_required_input(config, &optimistic, input.tokens)
                <= config.primary_context_tokens as f64
        });
    if !fits {
        return false;
    }
    let review = review_body(config, session);
    review_fits_measured(config, session, &review)
        && session
            .reviewer_input
            .known_estimate(
                &review,
                config
                    .classifier_url
                    .as_deref()
                    .unwrap_or(&config.upstream_url),
                config.model_resolution.as_ref().map(|p| &p.reviewer),
            )
            .is_some_and(|input| reviewer_rates(config, input.tokens as f64).is_some())
}
