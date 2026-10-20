// Independent-review counterexamples. Existing fixture admits protection through
// the production advice boundary; these are component gates, not native forks.
#[test]
fn lease_r1_inflight_annotations_cannot_force_following_primary_capacity_wait() {
    let (mut config, mut session, body) = lease_fixture();
    let base = review_body(&config, &session);
    config.classifier_max_input_tokens = reviewer_required_tokens(&config, &base).ceil() as u32;
    assert!(primary_fits(&config, &body));
    assert!(review_fits(&config, &base));
    session.background_review_pending = true;
    let targets = session.due_keep_targets();
    session.keep_leases.as_mut().unwrap().pending = Some(crate::proxy_state::PendingKeepReview {
        request_id: "held-review".into(),
        targets,
    });
    assert!(
        !review_fits(&config, &review_body(&config, &session)),
        "test must cross only the target-ledger boundary"
    );
    let (_, report) = plan(&config, &session, &body);
    assert_eq!(
        report["forced"], false,
        "LEASE_R1_RED: reviewer-only in-flight annotations cannot force a fitting following primary into capacity rescue/wait"
    );
}

#[test]
fn lease_r2_existing_cache_preserving_candidate_prevents_extra_paid_review() {
    let (mut config, mut session, _) = lease_fixture();
    config.payoff_requests = 5;
    let mut raw = session
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    raw.extend([
        json!({"role":"assistant","content":"eligible suffix ".repeat(1000)}),
        json!({"role":"user","content":"current task"}),
    ]);
    session.ingest(&raw).unwrap();
    for i in &mut session.history {
        i.exposed = true;
    }
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":["g1","g4"],"memories":[]}),
        )
        .unwrap();
    let groups = session.groups();
    admission::cover(&mut session, &groups);
    let body = json!({"model":"gpt-6-sol","input":session.render_primary()});
    let mut prefix = body.clone();
    prefix["input"] = json!(&body["input"].as_array().unwrap()[..3]);
    observe_cache(
        &mut session.primary_cache,
        &prefix,
        &json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":100}}),
    );
    let (candidate, _) = plan(&config, &session, &body);
    let candidate = candidate.expect("setup requires a real feasible paired candidate");
    assert_ne!(candidate.render_primary(), session.render_primary());
    assert_eq!(
        admission::decision(&config, &session, &body, false)["admitted"],
        false,
        "LEASE_R2_RED: an already-sufficient cache-preserving candidate cannot justify buying lease revalidation"
    );
}

#[test]
fn lease_r3_policy_change_preserves_group_memory_release_and_protected_memory() {
    let (_, mut session, _) = lease_fixture();
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":[],"memories":[
        {"source_ids":["g2"],"text":"released fact"},
        {"source_ids":["g2"],"text":"protected fact"}]}),
        )
        .unwrap();
    let released = format!("m{}", session.memories[0].id);
    let protected = format!("m{}", session.memories[1].id);
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[released.clone(),protected.clone()],"removable":[],"memories":[]}),
        )
        .unwrap();
    session.completed_requests += 8;
    let mut targets = session.due_keep_targets();
    targets.remove(&protected);
    session.resolve_keep_targets(&crate::proxy_state::PendingKeepReview {
        request_id: "accepted-targets".into(),
        targets,
    });
    let before = session.keep_leases.as_ref().unwrap().entries.clone();
    assert!(before["g2"].released_at_round.is_some());
    assert!(before[&released].released_at_round.is_some());
    assert!(before[&protected].released_at_round.is_none());
    let mut resumed: Session =
        serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
    resumed.initialize_keep_leases(3);
    let after = &resumed.keep_leases.as_ref().unwrap().entries;
    assert_eq!(
        after.get("g2"),
        before.get("g2"),
        "LEASE_R3_RED: durable released group eligibility cannot disappear on a supported turns override"
    );
    assert_eq!(after.get(&released), before.get(&released));
    assert!(after[&protected].released_at_round.is_none());
    assert_eq!(
        after[&protected].renewed_at_round,
        resumed.completed_requests
    );
    resumed.completed_requests += 3;
    assert!(resumed.due_keep_targets().contains_key(&protected));
}
