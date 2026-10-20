//! Shared ordinary-review policy. Estimates are scheduling heuristics, not native usage.
use super::*;
use crate::proxy_state::ReviewAdmission;
use std::collections::BTreeSet;

fn coverage(session: &Session) -> ReviewAdmission {
    session
        .review_admission
        .clone()
        .unwrap_or_else(|| ReviewAdmission {
            // Legacy observations already ingested native source. Never call replay
            // reconstruction "new growth" merely because this field was absent.
            source_ids: session.observed.keys().copied().collect(),
            no_yield_streak: 0,
        })
}

pub(super) fn initialize(session: &mut Session) {
    if session.review_admission.is_none() {
        session.review_admission = Some(coverage(session));
    }
}

fn source_ids(session: &Session) -> BTreeSet<u64> {
    session
        .groups()
        .iter()
        .filter(|g| g.replayable && g.exposed && !g.pinned)
        .flat_map(|g| g.members.iter().copied())
        .collect()
}

pub(super) fn multiplier(config: &ProxyCli, session: &Session) -> u64 {
    let mut value = 1u64;
    for _ in 0..coverage(session).no_yield_streak.min(64) {
        value = value
            .saturating_mul(config.review_backoff_factor as u64)
            .min(config.review_backoff_max as u64);
        if value == config.review_backoff_max as u64 || config.review_backoff_factor == 1 {
            break;
        }
    }
    value
}

pub(super) fn decision(
    config: &ProxyCli,
    session: &Session,
    main: &Value,
    pressure: bool,
) -> Value {
    decision_for_plan(config, session, main, pressure, true)
}

pub(super) fn decision_for_plan(
    config: &ProxyCli,
    session: &Session,
    main: &Value,
    pressure: bool,
    allow_extra: bool,
) -> Value {
    decision_with_primary(config, session, main, pressure, allow_extra, None)
}

pub(super) fn primary_tokens(config: &ProxyCli, session: &Session, main: &Value) -> f64 {
    #[cfg(test)]
    planner_work::record("admission_primary_projections");
    let mut retained = main.clone();
    retained["input"] = json!(session.render_primary());
    estimate(&primary_wire_body(config, &retained))
}

pub(super) fn decision_with_primary(
    config: &ProxyCli,
    session: &Session,
    main: &Value,
    pressure: bool,
    allow_extra: bool,
    primary: Option<f64>,
) -> Value {
    let coverage = coverage(session);
    let eligible = source_ids(session);
    // Each newly eligible native item contributes once, independent of repeated
    // ledgers, reviewer answers, memories or replay/cache cleanup epochs.
    let growth: f64 = session
        .history
        .iter()
        .filter(|i| eligible.contains(&i.id) && !coverage.source_ids.contains(&i.id))
        .map(|i| estimate(&i.value))
        .sum();
    let primary = primary.unwrap_or_else(|| primary_tokens(config, session, main));
    let age = session
        .completed_requests
        .saturating_sub(session.last_review_request);
    let multiplier = multiplier(config, session);
    let requests = config.review_every_requests.saturating_mul(multiplier);
    let tokens = (config.review_min_new_tokens as u64).saturating_mul(multiplier);
    let mut reason = if eligible.is_empty() && session.memories.is_empty() {
        "no_eligible_sources"
    } else if pressure {
        "pressure_override"
    } else if primary < config.review_min_primary_tokens as f64 {
        "primary_floor"
    } else if age < requests {
        "cadence"
    } else if growth < tokens as f64 {
        "native_source_growth"
    } else {
        "admitted"
    };
    if allow_extra
        && reason == "native_source_growth"
        && economic_keep_demand(config, session, main)
    {
        reason = "keep_lease_economic";
    }
    json!({"admitted": matches!(reason, "admitted" | "pressure_override" | "keep_lease_economic"),
        "reason":reason,"primary_estimated_tokens":primary,
        "new_native_source_estimated_tokens":growth,"completed_requests_since_review":age,
        "required_requests":requests,"required_new_tokens":tokens,
        "min_primary_tokens":config.review_min_primary_tokens,"multiplier":multiplier,
        "no_yield_streak":coverage.no_yield_streak,"covered_source_count":coverage.source_ids.len(),
        "eligible_source_count":eligible.len(),"lineage_rebases":session.history_rebases})
}

/// One bounded hypothetical release, using the very same removal, rendered
/// feasibility, cache/tier pricing and payback decision as the live planner.
/// No paid work is justified by elapsed rounds alone or speculative review wins.
fn economic_keep_demand(config: &ProxyCli, session: &Session, main: &Value) -> bool {
    if !session.extra_keep_attempt_available()
        || session.background_review_pending
        || session.bootstrap_review_pending
        || !standard(main)
    {
        return false;
    }
    // Do not buy advice when already-authorized material supplies a safe useful
    // candidate, including a cache-preserving subset. Probe the actual planner
    // with extra-lease admission disabled to break the admission/forecast cycle.
    if planning::economic_candidate(config, session, main, 0.0) {
        return false;
    }
    let mut reviewed = session.clone();
    let pending = crate::proxy_state::PendingKeepReview {
        request_id: "forecast".into(),
        targets: session.due_keep_targets(),
    };
    reviewed.keep_leases.as_mut().unwrap().pending = Some(pending.clone());
    let request = review_body(config, &reviewed);
    if !review_fits(config, &request) {
        return false;
    }
    let Some(rates) = Rates::for_model(&config.classifier_model, estimate(&request)) else {
        return false;
    };
    let Some(inputs) = reviewer_view_costs(config, &session.reviewer_cache, &request, &request, 1)
    else {
        return false;
    };
    let paid = inputs.keep + config.classifier_max_output_tokens as f64 * rates.output;
    reviewed.resolve_keep_targets(&pending);
    planning::economic_candidate(config, &reviewed, main, paid)
}

/// Classify actual committed deltas, not accepted-opinion counters (which include
/// repeats). New keeps are progress but not removal yield. Unique materialized
/// memories and newly authorized drops are useful even before a paid rewrite.
pub(super) fn outcome(before: &Session, after: &Session) -> &'static str {
    if after
        .groups()
        .iter()
        .any(|g| after.drop_authorized(g) && !before.drop_authorized(g))
        || after.memories.iter().any(|m| {
            !before
                .memories
                .iter()
                .any(|old| old.text == m.text && old.source_ids == m.source_ids)
        })
        || before
            .memories
            .iter()
            .any(|m| !after.memories.iter().any(|new| new.id == m.id))
    {
        "productive"
    } else if after.opinions != before.opinions {
        "protection_progress"
    } else {
        "no_yield"
    }
}

pub(super) fn complete(
    before: &Session,
    after: &mut Session,
    valid: bool,
    groups: &[Group],
) -> &'static str {
    initialize(after);
    if !valid {
        return "failed_or_invalid";
    }
    let outcome = outcome(before, after);
    cover(after, groups);
    let admission = after.review_admission.as_mut().unwrap();
    match outcome {
        "productive" => admission.no_yield_streak = 0,
        "no_yield" => {
            admission.no_yield_streak = admission.no_yield_streak.saturating_add(1).min(64)
        }
        _ => {}
    }
    outcome
}

pub(super) fn cover(session: &mut Session, groups: &[Group]) {
    initialize(session);
    let ids = groups
        .iter()
        .filter(|g| g.replayable && g.exposed && !g.pinned)
        .flat_map(|g| g.members.iter().copied());
    let admission = session.review_admission.as_mut().unwrap();
    admission.source_ids.extend(ids);
    // Keep only active history identity; removed IDs cannot re-enter this lineage.
    admission
        .source_ids
        .retain(|id| session.history.iter().any(|i| i.id == *id && !i.removed));
}
