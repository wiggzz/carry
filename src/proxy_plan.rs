//! Fixed-history primary/reviewer economics, separate from transport and state commits.
use super::{
    Group, HashMap, HashSet, JointDecision, ProxyCli, Rates, Session, Value, ViewCosts, cache_base,
    classifier_cache_enabled, estimate, joint_decision, json, now, primary_wire_body, reusable,
    review_body_with_input, review_fits, reviewer_reusable, reviewer_view_costs, select_removals,
    standard, view_costs,
};

/// Index canonical items once. Projecting a frozen partition then visits only
/// that partition's items, rather than cloning or rescanning the whole session.
pub(super) struct ReviewProjection<'a> {
    session: &'a Session,
    items: HashMap<u64, &'a crate::proxy_state::Item>,
    observations: HashMap<u64, Vec<&'a crate::proxy_state::ShadowRecord>>,
    task: Option<&'a crate::proxy_state::Item>,
}

impl<'a> ReviewProjection<'a> {
    pub(super) fn new(session: &'a Session) -> Self {
        let mut observations = HashMap::<_, Vec<_>>::new();
        for record in &session.active_shadow {
            observations
                .entry(record.source_id)
                .or_default()
                .push(record);
        }
        Self {
            session,
            items: session.history.iter().map(|item| (item.id, item)).collect(),
            observations,
            task: session
                .history
                .iter()
                .rev()
                .find(|item| !item.removed && item.value["role"] == "user"),
        }
    }

    pub(super) fn body(&self, config: &ProxyCli, groups: &[Group]) -> Value {
        let mut view = Session {
            history: self.task.into_iter().cloned().collect(),
            opinions: groups
                .iter()
                .filter_map(|group| {
                    self.session
                        .opinions
                        .get(&group.id)
                        .map(|opinion| (group.id, opinion.clone()))
                })
                .collect(),
            memories: self.session.memories.clone(),
            review_feedback: self.session.review_feedback.clone(),
            reviewer_cache: self.session.reviewer_cache.clone(),
            review_cache_key: self.session.review_cache_key.clone(),
            review_context: self.session.review_context.clone(),
            ..Session::default()
        };
        for group in groups {
            let observed = group.members.iter().all(|id| {
                self.session
                    .observed
                    .get(id)
                    .is_some_and(|owner| owner == &group.id.to_string())
            });
            if observed && let Some(records) = self.observations.get(&group.id) {
                view.active_shadow
                    .extend(records.iter().map(|record| (*record).clone()));
            } else {
                let items = group
                    .members
                    .iter()
                    .filter_map(|id| self.items.get(id))
                    .map(|item| item.value.clone())
                    .collect::<Vec<_>>();
                view.active_shadow.push(crate::proxy_state::ShadowRecord {
                    source_id: group.id,
                    value: json!({"role":"user","content":json!({"group_id":format!("g{}",group.id),"member_ids":group.members,"items":items,"human":group.human}).to_string()}),
                });
            }
        }
        if let Some(memories) = self.observations.get(&0) {
            view.active_shadow
                .extend(memories.iter().map(|record| (*record).clone()));
        }
        let input = view.shadow_input_with_sources(groups, groups);
        review_body_with_input(config, &view, input)
    }
}

pub(super) struct PricedReviewWindow {
    pub(super) groups: Vec<Group>,
    pub(super) body: Value,
    pub(super) added_tail: bool,
    pub(super) pending_body: Option<Value>,
}

pub(super) fn priced_review_window(
    config: &ProxyCli,
    projection: &ReviewProjection<'_>,
    groups: Vec<Group>,
    body: Value,
    added_tail: bool,
) -> Option<PricedReviewWindow> {
    let session = projection.session;
    let remaining = groups
        .iter()
        .filter(|group| {
            session.review_end_id != 0
                && group.members.iter().all(|id| *id <= session.review_end_id)
                && group
                    .members
                    .iter()
                    .any(|id| !session.reviewed_members.contains(id))
        })
        .cloned()
        .collect::<Vec<_>>();
    let pending_body = if remaining.iter().any(|group| group.exposed && !group.pinned) {
        let body = projection.body(config, &remaining);
        if !review_fits(config, &body) {
            return None;
        }
        Some(body)
    } else {
        None
    };
    Some(PricedReviewWindow {
        groups,
        body,
        added_tail,
        pending_body,
    })
}

fn bounded_review_tail(
    config: &ProxyCli,
    projection: &ReviewProjection<'_>,
    groups: &[Group],
    output: &mut Vec<PricedReviewWindow>,
) -> bool {
    if groups.is_empty() {
        return true;
    }
    let body = projection.body(config, groups);
    if review_fits(config, &body) {
        let Some(window) = priced_review_window(config, projection, groups.to_vec(), body, true)
        else {
            return false;
        };
        output.push(window);
        return true;
    }
    // Capacity is the only tail split rule. Bisection visits each source once
    // per tree level, without rerunning retention/economic window selection.
    if groups.len() == 1 {
        return false;
    }
    let middle = groups.len() / 2;
    bounded_review_tail(config, projection, &groups[..middle], output)
        && bounded_review_tail(config, projection, &groups[middle..], output)
}

pub(super) fn frozen_review_sweep(
    config: &ProxyCli,
    session: &Session,
    groups: &[Group],
    projection: &ReviewProjection<'_>,
) -> Option<Vec<PricedReviewWindow>> {
    if !groups.iter().any(|group| group.exposed && !group.pinned) && session.memories.is_empty() {
        return Some(Vec::new());
    }
    let full_window = || {
        let body = projection.body(config, groups);
        if !review_fits(config, &body) {
            return None;
        }
        priced_review_window(config, projection, groups.to_vec(), body, false)
            .map(|window| vec![window])
    };
    if session.reviewer_windows.is_empty() {
        return full_window();
    }
    let current = groups
        .iter()
        .map(|group| (group.id, group))
        .collect::<HashMap<_, _>>();
    let mut covered = HashSet::new();
    let mut windows = Vec::new();
    for frozen in &session.reviewer_windows {
        let mut selected = Vec::new();
        for old in frozen {
            if let Some(group) = current.get(&old.id) {
                if group.members != old.members {
                    return full_window();
                }
                covered.insert(group.id);
                selected.push((*group).clone());
            }
        }
        if selected.iter().any(|group| group.exposed && !group.pinned) {
            let body = projection.body(config, &selected);
            if !review_fits(config, &body) {
                return None;
            }
            windows.push(priced_review_window(
                config, projection, selected, body, false,
            )?);
        }
    }
    let tail = groups
        .iter()
        .filter(|group| group.exposed && !group.pinned && !covered.contains(&group.id))
        .cloned()
        .collect::<Vec<_>>();
    if !bounded_review_tail(config, projection, &tail, &mut windows) {
        return None;
    }
    if windows.is_empty() && !session.memories.is_empty() {
        let body = projection.body(config, &[]);
        if !review_fits(config, &body) {
            return None;
        }
        windows.push(priced_review_window(
            config,
            projection,
            Vec::new(),
            body,
            false,
        )?);
    }
    Some(windows)
}

#[derive(Default)]
pub(super) struct ReviewSchedule {
    pub(super) events: u64,
    pub(super) calls: Vec<u64>,
    pub(super) pending_calls: Vec<u64>,
}

pub(super) fn review_schedule(
    config: &ProxyCli,
    session: &Session,
    windows: &[PricedReviewWindow],
) -> ReviewSchedule {
    let mut schedule = ReviewSchedule {
        events: 0,
        calls: vec![0; windows.len()],
        pending_calls: vec![0; windows.len()],
    };
    let mut pending = if session.review_end_id != 0 {
        windows
            .iter()
            .enumerate()
            .filter(|(_, window)| {
                window.groups.iter().any(|group| {
                    group.exposed
                        && !group.pinned
                        && group.members.iter().all(|id| *id <= session.review_end_id)
                        && group
                            .members
                            .iter()
                            .any(|id| !session.reviewed_members.contains(id))
                })
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut initial_sweep = !pending.is_empty();
    let mut finish_empty_sweep = session.review_end_id != 0 && pending.is_empty();
    let mut elapsed = session
        .completed_requests
        .saturating_sub(session.last_review_request);
    for _ in 0..config.payoff_requests.saturating_sub(1) {
        elapsed = elapsed.saturating_add(1);
        if finish_empty_sweep {
            finish_empty_sweep = false;
            elapsed = 0;
            continue;
        }
        if pending.is_empty() {
            if elapsed < config.review_every_requests || windows.is_empty() {
                continue;
            }
            pending = (0..windows.len()).collect();
        }
        schedule.events += 1;
        let count = pending.len().min(config.review_max_calls as usize);
        for index in pending.drain(..count) {
            schedule.calls[index] += 1;
            schedule.pending_calls[index] += u64::from(initial_sweep);
        }
        if pending.is_empty() {
            initial_sweep = false;
        }
        elapsed = 0;
    }
    schedule
}

pub(super) fn sweep_cost(
    config: &ProxyCli,
    session: &Session,
    windows: &[PricedReviewWindow],
    schedule: &ReviewSchedule,
) -> Option<f64> {
    // The ordinary scheduler does not price unfinished bootstrap work or the
    // memory-only POST after an otherwise empty pending sweep. Do not treat
    // either future dispatch as free. payoff=1 has no future reviewer horizon.
    if config.payoff_requests > 1
        && ((session.bootstrap_review_pending && !session.bootstrap_review_complete)
            || (session.review_end_id != 0
                && !session.memories.is_empty()
                && schedule.pending_calls.iter().all(|calls| *calls == 0)))
    {
        return None;
    }
    windows.iter().zip(&schedule.calls).enumerate().try_fold(
        0.0,
        |total, (index, (window, calls))| {
            if *calls == 0 {
                return Some(total);
            }
            let cache = if window.added_tail {
                &[][..]
            } else {
                session.reviewer_cache.as_slice()
            };
            let pending_calls = schedule.pending_calls[index];
            let full_calls = calls.saturating_sub(pending_calls);
            let input =
                reviewer_view_costs(config, cache, &window.body, &window.body, full_calls)?.keep;
            let rates = Rates::for_model(&config.classifier_model, estimate(&window.body))?;
            let pending_cost = if pending_calls > 0 {
                let body = window.pending_body.as_ref()?;
                let rates = Rates::for_model(&config.classifier_model, estimate(body))?;
                reviewer_view_costs(config, cache, body, body, pending_calls)?.keep
                    + pending_calls as f64
                        * config.classifier_max_output_tokens as f64
                        * rates.output
            } else {
                0.0
            };
            Some(
                total
                    + input
                    + full_calls as f64 * config.classifier_max_output_tokens as f64 * rates.output
                    + pending_cost,
            )
        },
    )
}

pub(super) fn plan(config: &ProxyCli, session: &Session, body: &Value) -> (Option<Session>, Value) {
    let wire = primary_wire_body(config, body);
    if !standard(body)
        || Rates::for_model(wire["model"].as_str().unwrap_or(""), estimate(&wire)).is_none()
    {
        return (
            None,
            json!({"selected": false, "reason": "unsupported_model_or_tier"}),
        );
    }
    let groups = session.groups();
    let eligible = groups
        .iter()
        .filter(|g| session.drop_authorized(g))
        .map(|g| g.id)
        .collect::<HashSet<_>>();
    let mut rendered = session.render_primary_with_ids();
    let mut folded = HashSet::new();
    if config.codex_login {
        let leading = rendered
            .iter()
            .take_while(|(_, value)| matches!(value["role"].as_str(), Some("system" | "developer")))
            .count();
        folded.extend(rendered.drain(..leading).filter_map(|(id, _)| id));
    }
    let mut candidates = vec![HashSet::new()];
    for evidence in &session.primary_cache {
        if now().saturating_sub(evidence.at) <= 1800
            && wire["input"]
                .as_array()
                .is_some_and(|input| evidence.matches(&cache_base(&wire), input))
        {
            let frontier_ids = rendered
                .iter()
                .take(evidence.input_len())
                .filter_map(|(id, _)| *id)
                .chain(folded.iter().copied())
                .collect::<HashSet<_>>();
            candidates.push(
                groups
                    .iter()
                    .filter(|g| g.members.iter().any(|id| frontier_ids.contains(id)))
                    .map(|g| g.id)
                    .collect(),
            );
        }
    }
    let projection = ReviewProjection::new(session);
    let keep_windows = if config.payoff_requests == 1 {
        Vec::new()
    } else if let Some(windows) = frozen_review_sweep(config, session, &groups, &projection) {
        windows
    } else {
        return (
            None,
            json!({"selected":false,"reason":"reviewer_partition_unknown","reviewer_economics_priced":false,"partition_assumption":"completed_frozen_windows_required_for_oversized_history"}),
        );
    };
    let keep_schedule = review_schedule(config, session, &keep_windows);
    let Some(keep_shadow_cost) = sweep_cost(config, session, &keep_windows, &keep_schedule) else {
        return (
            None,
            json!({"selected":false,"reason":"reviewer_pricing_unknown","reviewer_economics_priced":false}),
        );
    };
    let mut reports = Vec::new();
    let mut reviewer_economics_priced = true;
    let mut best: Option<(Session, JointDecision)> = None;
    for protected in candidates {
        let removed = select_removals(groups.iter().map(|g| g.id), &eligible, &protected);
        if removed.is_empty() {
            continue;
        }
        let mut candidate = session.clone();
        candidate.remove(&removed, &groups);
        let mut compact = body.clone();
        compact["input"] = json!(candidate.render_primary());
        let compact = primary_wire_body(config, &compact);
        let Some(primary) = view_costs(
            &wire,
            &compact,
            &session.primary_cache,
            wire["model"].as_str().unwrap_or(""),
            config.payoff_requests,
        ) else {
            continue;
        };
        let candidate_groups = candidate.groups();
        let candidate_projection = ReviewProjection::new(&candidate);
        let compact_windows = if config.payoff_requests == 1 {
            Vec::new()
        } else if let Some(windows) =
            frozen_review_sweep(config, &candidate, &candidate_groups, &candidate_projection)
        {
            windows
        } else {
            reports.push(json!({"removed_groups":removed,"reason":"reviewer_partition_unknown"}));
            continue;
        };
        let compact_schedule = review_schedule(config, &candidate, &compact_windows);
        let Some(compact_shadow_cost) =
            sweep_cost(config, &candidate, &compact_windows, &compact_schedule)
        else {
            reviewer_economics_priced = false;
            reports.push(json!({"removed_groups":removed,"reason":"reviewer_pricing_unknown","reviewer_economics_priced":false}));
            continue;
        };
        let shadow = ViewCosts {
            keep: keep_shadow_cost,
            compact: compact_shadow_cost,
        };
        let decision = joint_decision(primary, shadow, config.min_payback_percent);
        reports.push(json!({
            "removed_groups": removed,
            "joint": decision,
            "primary_retained_estimated_tokens": estimate(&compact),
            "reviewer_sweep_estimated_input_tokens": compact_windows.iter().map(|window| estimate(&window.body)).sum::<f64>(),
            "primary_cached_estimated_tokens": reusable(&session.primary_cache, &compact),
            "reviewer_sweep_cached_estimated_input_tokens": compact_windows.iter().map(|window| reviewer_reusable(config, &session.reviewer_cache, &window.body)).sum::<f64>(),
            "future_review_events": compact_schedule.events,
            "future_reviewer_calls": compact_schedule.calls.iter().sum::<u64>()
        }));
        if decision.accepted
            && best
                .as_ref()
                .is_none_or(|(_, prior)| decision.savings > prior.savings)
        {
            best = Some((candidate, decision));
        }
    }
    let report = json!({
        "selected": best.is_some(),
        "mode": format!("{:?}", config.mode).to_lowercase(),
        "future_review_events": keep_schedule.events,
        "future_reviewer_calls": keep_schedule.calls.iter().sum::<u64>(),
        "reviewer_economics_priced": reviewer_economics_priced,
        "reviewer_windows": keep_windows.len(),
        "added_tail_windows": keep_windows.iter().filter(|window| window.added_tail).count(),
        "payoff_requests": config.payoff_requests,
        "min_payback_percent": config.min_payback_percent,
        "current_review_cost_is_sunk": true,
        "estimate_basis": "o200k_base_rendered_json_estimate_not_provider_tokens_or_overflow_evidence",
        "cache_basis": "exact_prefix_native_cached_fraction_not_guarantee",
        "reviewer_cache_basis": "one_prior_transmitted_stable_boundary_bounded_by_native_read_fraction_not_guaranteed_write",
        "classifier_cache_policy": format!("{:?}", config.classifier_cache_policy),
        "classifier_explicit_cache": classifier_cache_enabled(config).unwrap_or(false),
        "future_cache_assumption": "fixed_history_frozen_partitions_with_bounded_chronological_uncached_tail_sensitivity_not_forecast_or_global_optimum;reviewer_stable_prefix_only_never_ledger",
        "future_review_output_assumption": "configured_max_output_tokens_per_future_window_not_prediction",
        "candidates": reports
    });
    (best.map(|(session, _)| session), report)
}
