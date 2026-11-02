//! Shared ordinary-review policy. Estimates are scheduling heuristics, not native usage.
use super::*;
use crate::proxy_state::ReviewAdmission;
use std::collections::BTreeSet;

fn coverage(session: &Session) -> ReviewAdmission {
    session.review_admission.clone().unwrap_or_default()
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

/// One request-boundary scheduling decision for both candidate pricing and
/// dispatch. Caller metadata/output caps are not reviewer admission context;
/// primary_tokens renders canonical source, independent of selected omission.
pub(super) fn scheduled(config: &ProxyCli, session: &Session) -> Value {
    // Pending/bootstrap decisions do not depend on token quantities.
    let bypass = session.background_review_pending || session.bootstrap_review_pending;
    let mut result = decision(config, session, &session.review_context, bypass);
    if session.background_review_pending {
        result["admitted"] = json!(false);
        result["reason"] = json!("review_pending");
    } else if session.bootstrap_review_pending && result["reason"] != "no_eligible_sources" {
        let initial = !session.bootstrap_review_attempted;
        let due = result["completed_requests_since_review"].as_u64().unwrap()
            >= result["required_requests"].as_u64().unwrap();
        result["admitted"] = json!(initial || due);
        result["reason"] = json!(if initial || due {
            "replay_bootstrap"
        } else {
            "cadence"
        });
    }
    result
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
    _allow_extra: bool,
) -> Value {
    decision_with_primary(config, session, main, pressure, false, None)
}

fn primary_quantity(
    config: &ProxyCli,
    session: &Session,
    main: &Value,
) -> Option<crate::proxy_quantity::InputEstimate> {
    let mut retained = main.clone();
    retained["input"] = json!(session.render_primary());
    primary_input_estimate(config, session, &retained)
}

#[cfg(test)]
pub(super) fn primary_tokens(config: &ProxyCli, session: &Session, main: &Value) -> f64 {
    primary_quantity(config, session, main).map_or(0.0, |input| input.tokens as f64)
}

pub(super) fn decision_with_primary(
    config: &ProxyCli,
    session: &Session,
    main: &Value,
    pressure: bool,
    _allow_extra: bool,
    primary: Option<f64>,
) -> Value {
    let coverage = coverage(session);
    let eligible = source_ids(session);
    let age = session
        .completed_requests
        .saturating_sub(session.last_review_request);
    let multiplier = multiplier(config, session);
    let requests = config.review_every_requests.saturating_mul(multiplier);
    let tokens = (config.review_min_new_tokens as u64).saturating_mul(multiplier);
    // Cheap policy gates first. Diagnostics must not manufacture a reason to
    // tokenize an otherwise ineligible request's entire history.
    let mut reason = if eligible.is_empty() && session.memories.is_empty() {
        Some("no_eligible_sources")
    } else if pressure {
        Some("pressure_override")
    } else if age < requests {
        Some("cadence")
    } else {
        None
    };
    let quantity = if reason.is_none() && primary.is_none() && config.review_min_primary_tokens > 0
    {
        primary_quantity(config, session, main)
    } else {
        None
    };
    let primary = primary.or_else(|| quantity.map(|input| input.tokens as f64));
    let visible = if reason.is_none() && primary.is_none() && config.review_min_primary_tokens > 0 {
        let mut retained = main.clone();
        retained["input"] = json!(session.render_primary());
        session
            .primary_input
            .visible_prompt_tokens(&primary_wire_body(config, &retained))
            .map(|tokens| tokens as f64)
    } else {
        None
    };
    if reason.is_none()
        && primary.or(visible).unwrap_or(0.0) < config.review_min_primary_tokens as f64
    {
        reason = Some(if primary.is_none() {
            "primary_quantity_unknown"
        } else {
            "primary_floor"
        });
    }
    // Each newly eligible native item contributes once. Count only these
    // sections, and only when the growth floor can still affect admission.
    let new = session
        .history
        .iter()
        .filter(|item| eligible.contains(&item.id) && !coverage.source_ids.contains(&item.id))
        .collect::<Vec<_>>();
    let growth = if new.is_empty() {
        Some(0.0)
    } else if reason.is_none() && tokens > 0 {
        Some(
            new.iter()
                .map(|item| session.primary_input.visible_text_tokens(&item.value) as f64)
                .sum::<f64>(),
        )
    } else {
        None
    };
    if reason.is_none() && growth.is_some_and(|growth| growth < tokens as f64) {
        reason = Some("native_source_growth");
    }
    let reason = reason.unwrap_or("admitted");
    json!({"admitted": matches!(reason, "admitted" | "pressure_override"),
        "reason":reason,"primary_estimated_tokens":primary,
        "primary_input_basis":quantity.map(|input|input.basis),
        "primary_local_visible_text_tokens":visible,
        "source_growth_basis":"local_visible_text_not_encoded_native_payloads",
        "new_native_source_estimated_tokens":growth,"completed_requests_since_review":age,
        "required_requests":requests,"required_new_tokens":tokens,
        "min_primary_tokens":config.review_min_primary_tokens,"multiplier":multiplier,
        "no_yield_streak":coverage.no_yield_streak,"covered_source_count":coverage.source_ids.len(),
        "eligible_source_count":eligible.len(),"lineage_rebases":session.history_rebases})
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
        .retain(|id| session.history.iter().any(|i| i.id == *id));
}
