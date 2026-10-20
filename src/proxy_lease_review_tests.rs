// Independent-review counterexamples. Existing fixture admits protection through
// the production advice boundary; these are component gates, not native forks.
#[tokio::test]
async fn lease_never_dispatched_overcap_target_leaves_no_live_pending_annotation() {
    let directory = tempfile::tempdir().unwrap();
    let (mut config, mut session, body) = lease_fixture();
    config.review_min_new_tokens = 0;
    config.state_dir = directory.path().to_path_buf();
    config.classifier_url = Some("http://127.0.0.1:1".into());
    session.review_context = cache_base(&body);
    session.review_cache_key = format!("{:x}", Sha256::digest("carry-review-overcap"));
    let groups = session.groups();
    prepare_review_history(&mut session, &groups);
    config.classifier_max_input_tokens =
        reviewer_required_tokens(&config, &review_body(&config, &session)).ceil() as u32;
    let mut service = checkpoint_service(directory.path());
    service.config = config;
    let before = serde_json::to_value(&session).unwrap();
    assert!(
        !review(
            &service,
            "overcap",
            &mut session,
            &body,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    assert_eq!(
        session.shadow.calls, 0,
        "capacity rejection is before network dispatch"
    );
    assert!(
        session.keep_leases.as_ref().unwrap().pending.is_none(),
        "LEASE_CANCEL_PENDING_RED: a never-dispatched target cannot remain a live reviewer-tail annotation"
    );
    assert_eq!(session.opinions.get(&2).map(String::as_str), Some("keep"));
    assert_eq!(serde_json::to_value(&session).unwrap(), before);
}

#[tokio::test]
async fn lease_checkpoint_reservation_failure_cancels_pending_targets() {
    let directory = tempfile::tempdir().unwrap();
    let (mut config, mut session, body) = lease_fixture();
    config.review_min_new_tokens = 0;
    config.state_dir = directory.path().join("not-a-directory");
    std::fs::write(&config.state_dir, b"owned fixture obstruction").unwrap();
    config.classifier_url = Some("http://127.0.0.1:1".into());
    let mut service = checkpoint_service(directory.path());
    service.config = config;
    let service = Arc::new(service);
    let before = serde_json::to_value(&session).unwrap();
    dispatch_review_for(
        service.clone(),
        "failed-reservation".into(),
        &mut session,
        body,
        "target".into(),
    )
    .await;
    assert!(!session.background_review_pending);
    assert!(service.background_reviews.lock().await.is_empty());
    assert_eq!(session.shadow.calls, 0);
    assert!(
        session.keep_leases.as_ref().unwrap().pending.is_none(),
        "LEASE_CANCEL_PENDING_RED: failed durable reservation must cancel undispatched targets"
    );
    assert_eq!(session.opinions.get(&2).map(String::as_str), Some("keep"));
    assert_eq!(serde_json::to_value(&session).unwrap(), before);
}

#[tokio::test]
async fn lease_duplicate_dispatch_preserves_inflight_targets_and_attempt_allowance() {
    let directory = tempfile::tempdir().unwrap();
    let (mut config, mut session, body) = lease_fixture();
    config.review_min_new_tokens = 0;
    config.state_dir = directory.path().to_path_buf();
    config.classifier_url = Some("http://127.0.0.1:1".into());
    session.background_review_pending = true;
    session.reserve_extra_keep_attempt();
    let targets = session.due_keep_targets();
    assert!(!targets.is_empty());
    session.keep_leases.as_mut().unwrap().pending = Some(crate::proxy_state::PendingKeepReview {
        request_id: "legitimate-inflight-generation".into(),
        targets,
    });
    let mut service = checkpoint_service(directory.path());
    service.config = config;
    let service = Arc::new(service);
    let id = "duplicate-dispatch";
    assert!(service.background_reviews.lock().await.insert(id.into()));
    save(&service, id, &session).unwrap();
    let before = serde_json::to_value(&session).unwrap();
    dispatch_review_for(
        service.clone(),
        id.into(),
        &mut session,
        body,
        "never-owned-this-generation".into(),
    )
    .await;
    assert_eq!(serde_json::to_value(&session).unwrap(), before);
    assert_eq!(
        serde_json::to_value(load(&service, id).unwrap()).unwrap(),
        before
    );
    let running = service.background_reviews.lock().await;
    assert_eq!(running.len(), 1);
    assert!(running.contains(id));
}

#[tokio::test]
async fn lease_overcap_preparation_preserves_prior_pending_generation() {
    let directory = tempfile::tempdir().unwrap();
    let (mut config, mut session, body) = lease_fixture();
    config.review_min_new_tokens = 0;
    config.state_dir = directory.path().to_path_buf();
    config.classifier_url = Some("http://127.0.0.1:1".into());
    session.review_context = cache_base(&body);
    session.review_cache_key = format!("{:x}", Sha256::digest("carry-review-overcap-prior"));
    let groups = session.groups();
    prepare_review_history(&mut session, &groups);
    config.classifier_max_input_tokens =
        reviewer_required_tokens(&config, &review_body(&config, &session)).ceil() as u32;
    let targets = session.due_keep_targets();
    assert!(!targets.is_empty());
    session.keep_leases.as_mut().unwrap().pending = Some(crate::proxy_state::PendingKeepReview {
        request_id: "prior-pending-generation".into(),
        targets,
    });
    let before = serde_json::to_value(&session).unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config = config;
    let mut run = ReviewRun::default();
    assert!(
        !review(
            &service,
            "overcap-prior",
            &mut session,
            &body,
            false,
            &mut run
        )
        .await
    );
    assert!(run.receipts.is_empty());
    assert!(run.keep_review.is_none());
    assert_eq!(serde_json::to_value(&session).unwrap(), before);
}

#[test]
fn lease_planner_evaluates_identical_retirement_action_only_once() {
    let (mut config, mut session, body) = lease_fixture();
    config.review_min_new_tokens = 0;
    let groups = session.groups();
    let mut prior = review_source_input(&session, &groups);
    let source_len = prior.len() - 1;
    let source_sha256 = reviewer_serialized_identity(&prior[..source_len]).1;
    prior.push(json!({"role":"assistant","content":json!({"protected":["g2"],"removable":[],"memories":[]}).to_string()}));
    session.review_history = Some(crate::proxy_state::ReviewHistory {
        source_len,
        source_sha256,
        input: prior,
    });
    for length in 1..=body["input"].as_array().unwrap().len() {
        let mut prefix = body.clone();
        prefix["input"] = json!(&body["input"].as_array().unwrap()[..length]);
        observe_cache(
            &mut session.primary_cache,
            &prefix,
            &json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":100}}),
        );
    }
    let (_, report) = plan(&config, &session, &body);
    let actions = report["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| serde_json::from_value::<Vec<u64>>(row["removed_groups"].clone()).unwrap())
        .collect::<Vec<_>>();
    let distinct = actions.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        distinct.len(),
        1,
        "setup has only one authorized retirement action"
    );
    assert_eq!(
        actions.len(),
        distinct.len(),
        "LEASE_DUPLICATE_ACTIONS_RED: distinct cache frontiers yielding identical removals must not repeatedly rebuild/tokenize the same paired forecast"
    );
}

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

#[tokio::test]
async fn lease_r1_real_held_extra_review_next_primary_crosses_only_target_budget() {
    let directory = tempfile::tempdir().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed = calls.clone();
    let held = started.clone();
    let resume = release.clone();
    let router = Router::new()
        .route("/review", post(move || {
            let calls = observed.clone();
            let started = held.clone();
            let release = resume.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n > 0 { started.notify_one(); release.notified().await; }
                let advice = if n == 0 { json!({"protected":["g2"],"removable":[],"memories":[]}) }
                    else { json!({"protected":[],"removable":[],"memories":[]}) };
                axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}],"usage":{"input_tokens":100,"output_tokens":10}}))
            }
        }))
        .route("/primary", post(|| async {
            axum::Json(json!({"id":"primary_without_review_completion","status":"completed","output":[],"usage":{"input_tokens":100,"output_tokens":1}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (config, mut session, mut body) = lease_fixture();
    // Exposure/maturity are staged component inputs. Discard fixture opinion:
    // only the real HTTP review below may establish protection for this case.
    session.opinions.clear();
    session.keep_leases = None;
    let mut service = checkpoint_service(directory.path());
    service.config = config;
    service.config.state_dir = directory.path().to_path_buf();
    service.config.review_min_new_tokens = 0;
    service.config.classifier_url = Some(format!("{base}/review"));
    service.config.upstream_url = format!("{base}/primary");
    service.config.classifier_timeout_secs = 10;
    body["model"] = json!("gpt-6-sol");
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "lease-held-boundary".parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    assert!(
        review(
            &service,
            &id,
            &mut session,
            &body,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    assert_eq!(session.opinions.get(&2).map(String::as_str), Some("keep"));
    session.completed_requests += 8;
    service.config.review_min_new_tokens = 25000;
    let mut target = session.clone();
    let targets = target.due_keep_targets();
    target.keep_leases.as_mut().unwrap().pending = Some(crate::proxy_state::PendingKeepReview {
        request_id: "forecast".into(),
        targets,
    });
    service.config.classifier_max_input_tokens =
        reviewer_required_tokens(&service.config, &review_body(&service.config, &target)).ceil()
            as u32;
    let mut following = body.clone();
    following["instructions"] = json!("following task instruction ".repeat(3));
    let mut next = target.clone();
    next.review_context = cache_base(&following);
    assert!(!review_fits(
        &service.config,
        &review_body(&service.config, &next)
    ));
    next.keep_leases.as_mut().unwrap().pending = None;
    assert!(
        review_fits(&service.config, &review_body(&service.config, &next)),
        "only the in-flight target suffix crosses the reviewer bound"
    );
    assert!(primary_fits(&service.config, &following));
    save(&service, &id, &session).unwrap();
    let service = Arc::new(service);
    dispatch_review_for(
        service.clone(),
        id.clone(),
        &mut session,
        body,
        "held-extra".into(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        forward_request(
            service.clone(),
            headers,
            serde_json::to_vec(&following).unwrap().into(),
            false,
            "following-primary".into(),
        ),
    )
    .await;
    let response = result
        .expect("following fitting primary must not await held lease review")
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(
        Duration::from_secs(5),
        axum::body::to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["status"],
        "completed"
    );
    let saved = load(&service, &id).unwrap();
    let remained_pending = saved.background_review_pending;
    release.notify_one();
    assert!(remained_pending, "success must precede reviewer completion");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "no rescue review");
    provider.abort();
}
