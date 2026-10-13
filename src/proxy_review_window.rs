//! Choose complete source windows by retention boundaries and modeled cache economics.
use super::{
    Group, HashSet, ProxyCli, Rates, Session, Value, estimate, json, review_body_for_groups,
    review_fits, reviewer_boundary_matches, reviewer_input, reviewer_stable_end,
    reviewer_view_costs,
};

pub(super) struct ReviewWindow {
    pub(super) view: Session,
    pub(super) groups: Vec<Group>,
    pub(super) body: Value,
}

pub(super) fn window_score(
    config: &ProxyCli,
    session: &Session,
    body: &Value,
    groups: &[Group],
) -> Option<f64> {
    let source_ids = groups
        .iter()
        .filter(|g| g.exposed && !g.pinned)
        .flat_map(|g| g.members.iter().copied())
        .collect::<HashSet<_>>();
    let sources = session
        .history
        .iter()
        .filter(|item| source_ids.contains(&item.id))
        .map(|item| item.value.clone())
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return None;
    }
    let requests = 1 + config.payoff_requests.saturating_sub(1) / config.review_every_requests;
    let input = reviewer_view_costs(config, &session.reviewer_cache, body, body, requests)?.keep;
    let rates = Rates::for_model(&config.classifier_model, estimate(body))?;
    let output = requests as f64 * config.classifier_max_output_tokens as f64 * rates.output;
    // Amortized modeled dollars per unit of actionable source coverage. This
    // includes shared instructions, mutable ledger and each bounded output.
    Some((input + output) / estimate(&json!(sources)).max(1.0))
}

pub(super) fn select_review_window(
    config: &ProxyCli,
    session: &Session,
    remaining: &[Group],
) -> Option<ReviewWindow> {
    let mut view = session.clone();
    view.active_shadow.retain(|record| record.source_id == 0);
    view.observed.clear();
    let mut chunk = Vec::new();
    let mut selected: Option<(ReviewWindow, Option<f64>)> = None;
    let mut frontier = None;
    for (index, group) in remaining.iter().enumerate() {
        view.observe_groups(std::slice::from_ref(group));
        chunk.push(group.clone());
        let body = review_body_for_groups(config, &view, &chunk);
        if !review_fits(config, &body) {
            break;
        }
        // Retention changes and prior cached window ends are natural cut points.
        // A capacity frontier is also considered, but no group-count cap applies.
        let signal_boundary = remaining.get(index + 1).is_none_or(|next| {
            session.opinions.get(&group.id) != session.opinions.get(&next.id)
                || group.human != next.human
        });
        let cached_boundary = session.reviewer_cache.iter().any(|e| {
            e.input_len == reviewer_stable_end(&body)
                && reviewer_boundary_matches(&reviewer_input(&body).unwrap(), e)
        });
        frontier = Some(ReviewWindow {
            view: view.clone(),
            groups: chunk.clone(),
            body,
        });
        if signal_boundary || cached_boundary {
            let candidate = frontier.take().unwrap();
            let score = window_score(config, session, &candidate.body, &candidate.groups);
            let replace = selected
                .as_ref()
                .is_none_or(|(_, prior)| match (score, *prior) {
                    (Some(next), Some(old)) => next <= old,
                    _ => true, // Unknown economics retains maximal bounded coverage.
                });
            if replace {
                selected = Some((candidate, score));
            }
        }
    }
    if let Some(candidate) = frontier {
        let score = window_score(config, session, &candidate.body, &candidate.groups);
        let replace = selected
            .as_ref()
            .is_none_or(|(_, prior)| match (score, *prior) {
                (Some(next), Some(old)) => next <= old,
                _ => true,
            });
        if replace {
            selected = Some((candidate, score));
        }
    }
    selected.map(|(window, _)| window)
}
