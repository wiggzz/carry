// Additional executable boundary, recovery, state and forecast controls.
fn small_admission_config(config: &mut ProxyCli) {
    config.review_min_primary_tokens = 0;
    config.review_min_new_tokens = 0;
    config.review_every_requests = 1;
}

async fn ordinary(service: &Service, session: &mut Session) -> bool {
    let main = admission_main(session);
    review(
        service,
        "controls",
        session,
        &main,
        false,
        &mut ReviewRun::default(),
    )
    .await
}

#[tokio::test]
async fn adaptive_floor_equality_and_post_rewrite_reevaluation_use_retained_wire_prompt() {
    let (_dir, mut service, provider) =
        admission_fixture(json!({"protected":[],"removable":[],"memories":[]})).await;
    small_admission_config(&mut service.config);
    let mut session = admission_session(100, 1);
    let mut main = admission_main(&session);
    main["instructions"] = json!("retained instructions ".repeat(50));
    main["tools"] = json!([{"type":"function","name":"inspect","parameters":{"type":"object"}}]);
    let floor = estimate(&primary_wire_body(&service.config, &main)) as u32;
    service.config.review_min_primary_tokens = floor + 1;
    assert!(
        !review(
            &service,
            "equal",
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    assert_eq!(session.shadow.calls, 0);
    service.config.review_min_primary_tokens = floor;
    assert!(
        review(
            &service,
            "equal",
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    session.remove(&[2], &session.groups());
    session.completed_requests += 10;
    // The original caller body still has removed source; admission must not use it.
    assert!(
        !review(
            &service,
            "equal",
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    assert_eq!(session.shadow.calls, 1);
    provider.abort();
}

#[tokio::test]
async fn adaptive_native_growth_equality_and_cadence_are_conjunctive() {
    let (_dir, mut service, provider) =
        admission_fixture(json!({"protected":[],"removable":[],"memories":[]})).await;
    small_admission_config(&mut service.config);
    service.config.review_backoff_factor = 1;
    let mut session = admission_session(100, 1);
    assert!(ordinary(&service, &mut session).await);
    session.completed_requests += 100;
    service.config.review_min_new_tokens = 1;
    assert!(
        !ordinary(&service, &mut session).await,
        "age alone cannot manufacture source growth"
    );
    let mut input = session
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    input.push(json!({"role":"assistant","content":"new native output ".repeat(50)}));
    input.push(json!({"role":"user","content":"next task"}));
    session.ingest(&input).unwrap();
    // Newly arrived but unexposed output must not trigger a review.
    let decision = admission::decision(&service.config, &session, &admission_main(&session), false);
    assert!(
        decision["new_native_source_estimated_tokens"]
            .as_f64()
            .unwrap()
            < 100.0
    );
    for item in &mut session.history {
        item.exposed = true;
    }
    let growth = admission::decision(&service.config,&session,&admission_main(&session),false)["new_native_source_estimated_tokens"].as_f64().unwrap() as u32;
    service.config.review_min_new_tokens = growth + 1;
    assert!(!ordinary(&service, &mut session).await);
    service.config.review_min_new_tokens = growth;
    service.config.review_every_requests = 100;
    assert!(
        ordinary(&service, &mut session).await,
        "exact equality satisfies both batches"
    );
    assert_eq!(session.shadow.calls, 2);
    provider.abort();
}

#[tokio::test]
async fn adaptive_repeated_drops_are_no_yield_backoff_caps_and_new_material_resets() {
    let advice = json!({"protected":[],"removable":["g2"],"memories":[]});
    let (_dir, mut service, provider) = admission_fixture(advice).await;
    small_admission_config(&mut service.config);
    let mut session = admission_session(100, 1);
    for expected in [1, 2, 4, 8, 8] {
        assert!(ordinary(&service, &mut session).await);
        assert_eq!(admission::multiplier(&service.config, &session), expected);
        assert!(session.drop_authorized(&session.groups()[1]));
        session.completed_requests += expected;
    }
    provider.abort();
    let (_dir2, service2, provider2)=admission_fixture(json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"unique durable material"}]})).await;
    service.config.classifier_url = service2.config.classifier_url.clone();
    assert!(ordinary(&service, &mut session).await);
    assert_eq!(admission::multiplier(&service.config, &session), 1);
    assert_eq!(session.memories.len(), 1);
    session.completed_requests += 1;
    assert!(ordinary(&service, &mut session).await);
    assert_eq!(
        session.memories.len(),
        1,
        "repeated memory is not new material"
    );
    assert_eq!(admission::multiplier(&service.config, &session), 2);
    provider2.abort();
}

#[tokio::test]
async fn adaptive_new_protection_holds_backoff_but_repeated_protection_is_no_yield() {
    let (_dir, mut service, provider) =
        admission_fixture(json!({"protected":["g2"],"removable":[],"memories":[]})).await;
    small_admission_config(&mut service.config);
    let mut session = admission_session(100, 1);
    admission::initialize(&mut session);
    session.review_admission.as_mut().unwrap().no_yield_streak = 2;
    session.completed_requests = 4;
    assert!(ordinary(&service, &mut session).await);
    assert_eq!(admission::multiplier(&service.config, &session), 4);
    session.completed_requests += 4;
    assert!(ordinary(&service, &mut session).await);
    assert_eq!(admission::multiplier(&service.config, &session), 8);
    assert!(!session.drop_authorized(&session.groups()[1]));
    provider.abort();
}

#[tokio::test]
async fn adaptive_invalid_or_failed_review_does_not_consume_source_or_add_no_yield() {
    for advice in [
        json!({"invalid":true}),
        json!({"protected":[],"removable":["g999"],"memories":[]}),
    ] {
        let (_dir, mut service, provider) = admission_fixture(advice).await;
        small_admission_config(&mut service.config);
        service.config.review_min_new_tokens = 10;
        let mut session = admission_session(100, 1);
        assert!(!ordinary(&service, &mut session).await);
        assert_eq!(
            session.review_admission.as_ref().unwrap().no_yield_streak,
            0
        );
        assert!(
            session
                .review_admission
                .as_ref()
                .unwrap()
                .source_ids
                .is_empty()
        );
        assert_eq!(session.last_review_request, 1);
        session.completed_requests += 1;
        assert!(!ordinary(&service, &mut session).await);
        assert_eq!(
            session.shadow.calls, 2,
            "invalid review retries after normal cadence without invented new source"
        );
        provider.abort();
    }
    let (_dir, mut service, provider) = admission_fixture(json!({})).await;
    provider.abort();
    service.config.classifier_url = Some("http://127.0.0.1:1/responses".into());
    small_admission_config(&mut service.config);
    let mut session = admission_session(100, 1);
    assert!(!ordinary(&service, &mut session).await);
    assert_eq!(session.shadow.unavailable_cost_calls, 1);
    assert_eq!(
        session.review_admission.as_ref().unwrap().no_yield_streak,
        0
    );
    assert!(
        session
            .review_admission
            .as_ref()
            .unwrap()
            .source_ids
            .is_empty()
    );
}

#[tokio::test]
async fn adaptive_bootstrap_and_pressure_bypass_only_ordinary_admission() {
    let (_dir, service, provider) =
        admission_fixture(json!({"protected":[],"removable":[],"memories":[]})).await;
    let mut session = admission_session(100, 0);
    session.import_replayed_prefix();
    let main = admission_main(&session);
    assert!(
        review(
            &service,
            "bootstrap",
            &mut session,
            &main,
            true,
            &mut ReviewRun::default()
        )
        .await
    );
    assert!(session.bootstrap_review_complete);
    assert_eq!(session.shadow.calls, 1);
    let mut pressure = admission_session(100, 0);
    let main = admission_main(&pressure);
    assert!(
        review(
            &service,
            "pressure",
            &mut pressure,
            &main,
            false,
            &mut ReviewRun {
                pressure: true,
                ..ReviewRun::default()
            }
        )
        .await
    );
    assert_eq!(pressure.shadow.calls, 1);
    // A pressure flag is not permission to review unexposed/pinned-only history.
    let mut pinned = Session::default();
    pinned
        .ingest(&[json!({"role":"user","content":"only current goal"})])
        .unwrap();
    let main = admission_main(&pinned);
    assert!(
        !review(
            &service,
            "pinned",
            &mut pinned,
            &main,
            false,
            &mut ReviewRun {
                pressure: true,
                ..ReviewRun::default()
            }
        )
        .await
    );
    assert_eq!(pinned.shadow.calls, 0);
    provider.abort();
}

#[test]
fn adaptive_legacy_migration_cleanup_restart_and_rebase_do_not_duplicate_source_growth() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    small_admission_config(&mut config);
    config.review_min_new_tokens = 1;
    let mut session = admission_session(100, 10);
    session.observe_groups(&session.groups());
    let mut legacy = serde_json::to_value(&session).unwrap();
    legacy.as_object_mut().unwrap().remove("review_admission");
    let mut session: Session = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        admission::decision(&config, &session, &admission_main(&session), false)["new_native_source_estimated_tokens"],
        0.0
    );
    admission::initialize(&mut session);
    session.compact_reviewer();
    session.observe_groups(&session.groups());
    let resumed: Session = serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
    assert_eq!(
        admission::decision(&config, &resumed, &admission_main(&resumed), false)["new_native_source_estimated_tokens"],
        0.0
    );
    session.review_admission.as_mut().unwrap().no_yield_streak = 3;
    session
        .ingest_with_rebase(&[json!({"role":"user","content":"new branch"})], true)
        .unwrap();
    assert_eq!(admission::multiplier(&config, &session), 1);
    assert!(
        session
            .review_admission
            .as_ref()
            .unwrap()
            .source_ids
            .is_empty()
    );
    assert_eq!(session.last_review_request, session.completed_requests);
}

#[test]
fn adaptive_planner_consumes_frozen_growth_once_and_uses_candidate_primary_size() {
    let mut config =
        ProxyCli::try_parse_from(["carry proxy", "--min-payback-percent", "0"]).unwrap();
    small_admission_config(&mut config);
    config.review_min_new_tokens = 10;
    config.payoff_requests = 5;
    let mut session = admission_session(1000, 1);
    let main = admission_main(&session);
    let (_, report) = plan(&config, &session, &main);
    assert_eq!(
        report["future_reviewer_calls"], 1,
        "a frozen source provides only one nonzero growth batch"
    );
    let groups = session.groups();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":["g2"],"memories":[]}),
        )
        .unwrap();
    config.review_min_primary_tokens = estimate(&main) as u32;
    let (_, report) = plan(&config, &session, &main);
    let removed = report["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["removed_groups"] == json!([2]))
        .unwrap();
    assert_eq!(
        removed["future_reviewer_compact_calls"], 0,
        "candidate retained primary is below floor even when original caller body is large"
    );
    assert_eq!(report["future_reviewer_calls"], 1);
}

#[tokio::test]
async fn adaptive_background_dispatch_and_compatible_merge_persist_real_backoff() {
    let (_dir, mut service, provider) =
        admission_fixture(json!({"protected":[],"removable":[],"memories":[]})).await;
    small_admission_config(&mut service.config);
    service.config.background_reviews = true;
    let service = Arc::new(service);
    let mut session = admission_session(100, 1);
    save(&service, "background", &session).unwrap();
    let main = admission_main(&session);
    dispatch_review(service.clone(), "background".into(), &mut session, main).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while service
            .background_reviews
            .lock()
            .await
            .contains("background")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let current = load(&service, "background").unwrap();
    assert_eq!(current.shadow.calls, 1);
    assert_eq!(admission::multiplier(&service.config, &current), 2);
    assert!(!current.background_review_pending);
    assert!(
        !current
            .review_admission
            .as_ref()
            .unwrap()
            .source_ids
            .is_empty()
    );
    provider.abort();
}
