use super::*;

fn lease_fixture() -> (ProxyCli, Session, Value) {
    let mut config = reviewer_config();
    config.review_min_new_tokens = 25_000;
    config.review_min_primary_tokens = 0;
    config.min_payback_percent = 0;
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"goal"}),
            json!({"role":"assistant","content":"old evidence ".repeat(6000)}),
            json!({"role":"user","content":"current task"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.initialize_keep_leases(config.keep_lease_turns);
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":["g2"],"removable":[],"memories":[]}),
        )
        .unwrap();
    session.observe_groups(&session.groups());
    let groups = session.groups();
    admission::cover(&mut session, &groups);
    session.completed_requests = 8;
    let body = json!({"model":"gpt-6-luna","input":session.render_primary()});
    (config, session, body)
}

#[test]
fn lease_economic_demand_can_admit_one_review_without_new_source_growth() {
    let (config, session, body) = lease_fixture();
    assert!(!session.drop_authorized(&session.groups()[1]));
    assert_eq!(
        admission::decision(&config, &session, &body, false)["new_native_source_estimated_tokens"],
        0.0
    );
    assert_eq!(
        admission::decision(&config, &session, &body, false)["admitted"],
        true,
        "LEASE_ECON_RED: a bounded due review should unlock an otherwise profitable safe source rewrite"
    );
    let (_, report) = plan(&config, &session, &body);
    assert_eq!(
        report["future_reviewer_calls"], 1,
        "paired forecast must count the same actual due lease review, not recurring phantom reviews"
    );
    assert!(!session.history[1].removed);
}

#[test]
fn lease_no_profit_or_preexpiry_never_admits_extra_paid_review() {
    let (mut config, mut session, body) = lease_fixture();
    config.min_payback_percent = 100;
    assert_eq!(
        admission::decision(&config, &session, &body, false)["admitted"],
        false
    );
    config.min_payback_percent = 0;
    session.completed_requests = 7;
    assert_eq!(
        admission::decision(&config, &session, &body, false)["admitted"],
        false
    );
}

#[test]
fn lease_renewal_generation_survives_older_target_omission_and_restart() {
    let (config, mut session, _) = lease_fixture();
    let pending = crate::proxy_state::PendingKeepReview {
        request_id: "old-review".into(),
        targets: session.due_keep_targets(),
    };
    session.completed_requests = 10;
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":["g2"],"removable":[],"memories":[]}),
        )
        .unwrap();
    session.resolve_keep_targets(&pending);
    assert_eq!(session.opinions.get(&2).map(String::as_str), Some("keep"));
    let mut restarted: Session =
        serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
    restarted.initialize_keep_leases(config.keep_lease_turns);
    restarted.completed_requests = 17;
    assert!(restarted.due_keep_targets().is_empty());
    restarted.completed_requests = 18;
    assert_eq!(restarted.due_keep_targets().len(), 1);
}

#[test]
fn lease_generation_exhaustion_cannot_turn_a_fresh_keep_into_release() {
    let (_, mut session, _) = lease_fixture();
    let pending = crate::proxy_state::PendingKeepReview {
        request_id: "last-generation".into(),
        targets: session.due_keep_targets(),
    };
    session.keep_leases.as_mut().unwrap().next_generation = u64::MAX;
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":["g2"],"removable":[],"memories":[]}),
        )
        .unwrap();
    session.resolve_keep_targets(&pending);
    assert_eq!(
        session.opinions.get(&2).map(String::as_str),
        Some("keep"),
        "LEASE_GENERATION_RED: identity exhaustion must fail closed, not release explicit renewal"
    );
    assert!(!session.drop_authorized(&session.groups()[1]));
}

#[test]
fn lease_humans_pins_unexposed_changed_content_and_lineage_stay_protected() {
    let (_, session, _) = lease_fixture();
    for mutation in 0..5 {
        let mut current = session.clone();
        let pending = crate::proxy_state::PendingKeepReview {
            request_id: "old-review".into(),
            targets: current.due_keep_targets(),
        };
        match mutation {
            0 => current.history[1].value["role"] = json!("user"),
            1 => current.history[1].exposed = false,
            2 => current.history[1].value["content"] = json!("changed"),
            3 => {
                current.history[1].value["content"] =
                    json!([{"type":"image","image_url":"synthetic"}])
            }
            _ => current.history_rebases += 1,
        }
        current.resolve_keep_targets(&pending);
        assert_eq!(current.opinions.get(&2).map(String::as_str), Some("keep"));
        assert!(!current.drop_authorized(&current.groups()[1]));
    }
}

#[test]
fn lease_legacy_keeps_start_fresh_and_released_eligibility_is_durable() {
    let (config, mut session, _) = lease_fixture();
    session.keep_leases = None;
    session.completed_requests = 100;
    session.initialize_keep_leases(config.keep_lease_turns);
    session.completed_requests = 107;
    assert!(session.due_keep_targets().is_empty());
    session.completed_requests = 108;
    let pending = crate::proxy_state::PendingKeepReview {
        request_id: "accepted-empty".into(),
        targets: session.due_keep_targets(),
    };
    assert_eq!(pending.targets.len(), 1);
    session.resolve_keep_targets(&pending);
    assert!(!session.history[1].removed, "release is not deletion");
    assert!(
        !session.drop_members.contains_key(&2),
        "not model-authored drop advice"
    );
    let mut resumed: Session =
        serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
    resumed.initialize_keep_leases(config.keep_lease_turns);
    assert!(resumed.drop_authorized(&resumed.groups()[1]));
    let groups = resumed.groups();
    resumed.remove(&[2], &groups);
    resumed.initialize_keep_leases(config.keep_lease_turns);
    assert!(resumed.keep_leases.as_ref().unwrap().entries.is_empty());
    let raw = resumed
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    resumed.ingest(&raw).unwrap();
    assert!(
        !resumed.render_primary().iter().any(|i| i == &raw[1]),
        "no raw source resurrection"
    );
}

#[test]
fn lease_due_targets_are_bounded_and_never_include_default_human_keeps() {
    let mut session = Session::default();
    let mut input = vec![json!({"role":"user","content":"human goal"})];
    for _ in 0..10 {
        input.push(json!({"role":"assistant","content":"evidence"}));
        input.push(json!({"role":"user","content":"human instruction"}));
    }
    session.ingest(&input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.initialize_keep_leases(8);
    let ids = session
        .groups()
        .iter()
        .map(|g| format!("g{}", g.id))
        .collect::<Vec<_>>();
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":ids,"removable":[],"memories":[]}),
        )
        .unwrap();
    session.completed_requests = 8;
    let due = session.due_keep_targets();
    assert_eq!(due.len(), 4);
    for (id, _) in due {
        let group = session
            .groups()
            .into_iter()
            .find(|g| id == format!("g{}", g.id))
            .unwrap();
        assert!(!group.human);
        assert!(
            !session.drop_authorized(&group),
            "due alone never authorizes removal"
        );
    }
}

#[test]
fn lease_protected_derived_memory_gets_its_own_revalidation_not_raw_resurrection() {
    let (config, mut session, body) = lease_fixture();
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":["g2"],
        "memories":[{"source_ids":["g2"],"text":"durable compact finding"}]}),
        )
        .unwrap();
    let (candidate, _) = plan(&config, &session, &body);
    session = candidate.expect("actual existing planner must safely remove the source first");
    assert!(session.history[1].removed);
    let id = format!("m{}", session.memories[0].id);
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[id.clone()],"removable":[],"memories":[]}),
        )
        .unwrap();
    session.completed_requests += 8;
    let pending = crate::proxy_state::PendingKeepReview {
        request_id: "memory-review".into(),
        targets: session.due_keep_targets(),
    };
    assert!(
        pending.targets.contains_key(&id),
        "LEASE_MEMORY_RED: protected derived memory needs an independent content-bound lease"
    );
    session.resolve_keep_targets(&pending);
    assert_eq!(
        session.memories.len(),
        1,
        "omission only releases, not immediate deletion"
    );
    let mut resumed: Session =
        serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
    resumed.initialize_keep_leases(config.keep_lease_turns);
    let body = json!({"model":"gpt-6-luna","input":resumed.render_primary()});
    let (candidate, _) = plan(&config, &resumed, &body);
    let compact = candidate.expect("profitable released memory can retire economically");
    assert!(compact.memories.is_empty());
    assert!(compact.history[1].removed);
    assert!(
        !compact
            .render_primary()
            .iter()
            .any(|i| i == &session.history[1].value)
    );
}
