// Ordinary policy controls independent of deleted import/replay/lease seams.
#[tokio::test]
async fn ordinary_review_uses_receipt_anchored_reasoning_forecast_after_bootstrap_and_restart() {
    let (url, mut captures, provider) = review_capture_provider().await;
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.classifier_url = Some(url);
    service.config.review_min_primary_tokens = 150_000;
    service.config.review_min_new_tokens = 25_000;
    service.config.review_every_requests = 4;
    service.config.review_backoff_factor = 1;
    let mut session = Session::default();
    let mut main = json!({"model":"gpt-6-luna","input":[
        {"role":"user","content":"original goal"},
        {"type":"reasoning","summary":[],"encrypted_content":"old synthetic".repeat(1000)},
        {"type":"function_call","call_id":"old","name":"inspect","arguments":"{}"},
        {"type":"function_call_output","call_id":"old","output":"old evidence"},
        {"role":"user","content":"current goal"}
    ]});
    session.ingest(main["input"].as_array().unwrap()).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.review_context = cache_base(&main);
    session.bootstrap_review_pending = true;
    assert_eq!(
        admission::scheduled(&service.config, &session)["reason"],
        "replay_bootstrap"
    );
    let projection = planning::Projection::canonical(&session);
    assert!(
        run_review(
            &service,
            "shared-sizing",
            &mut session,
            &projection,
            &mut ReviewRun::default()
        )
        .await
    );
    captures.recv().await.unwrap();
    assert_eq!(session.shadow.calls, 1);
    // The ingress owner applies completed advice after a successful review;
    // run_review alone deliberately cannot retire bootstrap intent.
    semantic::Record::capture(&session).apply(&mut session);
    assert!(!session.bootstrap_review_pending);
    // Native primary quantity belongs to this exact primary route and wire,
    // independently of the reviewer's just-observed 10k receipt.
    session.primary_input.observe(
        &primary_wire_body(&service.config, &main),
        &json!({"input_tokens":200000}),
        &service.config.upstream_url,
        service.config.model_resolution.as_ref().map(|p| &p.primary),
        100,
    );
    main["input"].as_array_mut().unwrap().extend([
        json!({"type":"reasoning","summary":[],"encrypted_content":"fresh synthetic".repeat(1000)}),
        json!({"type":"function_call","call_id":"fresh","name":"inspect","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"fresh","output":"new visible evidence ".repeat(9000)}),
        json!({"role":"user","content":"continue"}),
    ]);
    session.ingest(main["input"].as_array().unwrap()).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.completed_requests = session.last_review_request + 4;
    for restart in [false, true] {
        if restart {
            session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
        }
        let wire = primary_wire_body(&service.config, &main);
        let resolved = service.config.model_resolution.as_ref().map(|p| &p.primary);
        let forecast = session
            .primary_input
            .forecast_estimate(&wire, &service.config.upstream_url, resolved)
            .unwrap();
        assert_eq!(
            forecast.basis,
            crate::proxy_quantity::EstimateBasis::ReceiptAnchoredReasoningForecast
        );
        assert!(
            session
                .primary_input
                .estimate(&wire, &service.config.upstream_url, resolved)
                .is_none()
        );
        let frozen = serde_json::to_value(&session).unwrap();
        let decision = admission::scheduled(&service.config, &session);
        assert!(session.primary_input.visible_text_tokens(&main["input"][7]) >= 25000);
        assert!(
            session
                .groups()
                .iter()
                .any(|g| g.members.contains(&8) && g.replayable && g.exposed && !g.pinned)
        );
        assert_eq!(
            decision["primary_input_basis"], "receipt_anchored_reasoning_forecast",
            "ordinary review must share qualified economic sizing; restart={restart}: {decision}"
        );
        assert_eq!(
            decision["primary_estimated_tokens"].as_f64(),
            Some(forecast.tokens as f64)
        );
        assert_eq!(decision["admitted"], true, "{decision}");
        let (_, report) = plan(&service.config, &session, &main);
        assert_eq!(report["primary_estimated_tokens"], forecast.tokens);
        assert_eq!(
            report["reviewer_fits"], true,
            "planner must price the due review: {report}"
        );
        assert!(report["primary_required_tokens"].is_null());
        assert!(report["primary_fits"].is_null());
        let review = review_body_projected(
            &service.config,
            &session,
            &planning::Projection::canonical(&session),
        );
        let reviewer_quantity = session.reviewer_input.known_estimate(
            &review,
            service.config.classifier_url.as_deref().unwrap(),
            service.config.model_resolution.as_ref().map(|p| &p.reviewer),
        );
        assert_eq!(
            report["reviewer_estimated_tokens"],
            json!(reviewer_quantity.map(|input| input.tokens)),
            "reviewer sizing must use its own rendered body, route and observations"
        );
        assert_eq!(serde_json::to_value(&session).unwrap(), frozen);
    }
    let projection = planning::Projection::canonical(&session);
    let expected = review_body_projected(&service.config, &session, &projection);
    let service = Arc::new(service);
    dispatch_review_for(
        service.clone(),
        "shared-sizing".into(),
        &mut session,
        projection,
        "ordinary".into(),
    )
    .await;
    let actual = tokio::time::timeout(Duration::from_secs(5), captures.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        actual, expected,
        "planner and real ordinary reviewer must render the same body"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while service
            .background_reviews
            .lock()
            .await
            .contains("shared-sizing")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        load_existing(&service, "shared-sizing")
            .unwrap()
            .shadow
            .calls,
        2
    );
    provider.abort();
}

fn admission_subject() -> Session {
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"original goal"}),
            json!({"role":"assistant","content":"native evidence ".repeat(100)}),
            json!({"role":"user","content":"current goal"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.completed_requests = 4;
    session
}

fn admission_main(session: &Session) -> Value {
    json!({"model":"gpt-6-luna","input":session.render_primary()})
}

#[test]
fn ordinary_admission_floor_age_and_unique_native_growth_are_conjunctive() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    let mut session = admission_subject();
    let main = admission_main(&session);
    let primary = admission::primary_tokens(&config, &session, &main) as u32;
    config.review_min_primary_tokens = 0;
    let growth =
        admission::decision(&config, &session, &main, false)["new_native_source_estimated_tokens"]
            .as_f64()
            .unwrap() as u32;
    assert!(growth > 0);
    config.review_min_primary_tokens = primary + 1;
    config.review_min_new_tokens = growth;
    assert_eq!(
        admission::decision(&config, &session, &main, false)["reason"],
        "primary_floor"
    );
    config.review_min_primary_tokens = primary;
    session.completed_requests = 3;
    assert_eq!(
        admission::decision(&config, &session, &main, false)["reason"],
        "cadence"
    );
    session.completed_requests = 400;
    config.review_min_new_tokens = growth + 1;
    assert_eq!(
        admission::decision(&config, &session, &main, false)["reason"],
        "native_source_growth"
    );
    config.review_min_new_tokens = growth;
    assert_eq!(
        admission::decision(&config, &session, &main, false)["reason"],
        "admitted"
    );
    let groups = session.groups();
    admission::cover(&mut session, &groups);
    for resumed in [
        session.clone(),
        serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap(),
    ] {
        let decision = admission::decision(&config, &resumed, &main, false);
        assert_eq!(decision["new_native_source_estimated_tokens"], 0.0);
        assert_eq!(decision["reason"], "native_source_growth");
    }
}

#[test]
fn ordinary_backoff_classifies_new_protection_repeats_and_useful_removal() {
    let config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    let mut session = admission_subject();
    for expected in [2, 4, 8, 8] {
        let before = session.clone();
        let groups = session.groups();
        assert_eq!(
            admission::complete(&before, &mut session, true, &groups),
            "no_yield"
        );
        assert_eq!(admission::multiplier(&config, &session), expected);
    }
    let before = session.clone();
    let groups = session.groups();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[2],"removable":[],"memories":[]}),
        )
        .unwrap();
    assert_eq!(
        admission::complete(&before, &mut session, true, &groups),
        "protection_progress"
    );
    assert_eq!(admission::multiplier(&config, &session), 8);
    let before = session.clone();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[2],"memories":[]}),
        )
        .unwrap();
    assert_eq!(
        admission::complete(&before, &mut session, true, &groups),
        "productive"
    );
    assert_eq!(admission::multiplier(&config, &session), 1);
    let before = session.clone();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[2],"memories":[]}),
        )
        .unwrap();
    assert_eq!(
        admission::complete(&before, &mut session, true, &groups),
        "no_yield"
    );
    assert_eq!(admission::multiplier(&config, &session), 2);
    let restored: Session =
        serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
    assert_eq!(admission::multiplier(&config, &restored), 2);
}

#[test]
fn invalid_review_does_not_cover_source_or_increase_backoff_and_unexposed_source_cannot_admit() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    config.review_min_primary_tokens = 0;
    config.review_min_new_tokens = 1;
    let before = admission_subject();
    let mut after = before.clone();
    let groups = after.groups();
    assert_eq!(
        admission::complete(&before, &mut after, false, &groups),
        "failed_or_invalid"
    );
    let coverage = after.review_admission.as_ref().unwrap();
    assert_eq!(coverage.no_yield_streak, 0);
    assert!(coverage.source_ids.is_empty());
    assert_eq!(
        admission::decision(&config, &after, &admission_main(&after), false)["reason"],
        "admitted"
    );
    for item in &mut after.history {
        item.exposed = false;
    }
    let decision = admission::decision(&config, &after, &admission_main(&after), false);
    assert_eq!(decision["new_native_source_estimated_tokens"], 0.0);
    assert_eq!(decision["reason"], "no_eligible_sources");
}

#[test]
fn unknown_primary_size_is_optional_but_cadence_growth_and_pending_still_gate_review() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    config.review_min_primary_tokens = 150_000;
    config.review_min_new_tokens = 25_000;
    let mut session = Session::default();
    session.ingest(&[
        json!({"role":"user","content":"goal"}),
        json!({"type":"reasoning","summary":[],"encrypted_content":"unknown synthetic".repeat(1000)}),
        json!({"type":"function_call","call_id":"unknown","name":"inspect","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"unknown","output":"visible evidence ".repeat(13000)}),
        json!({"role":"user","content":"continue"}),
    ]).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.completed_requests = 4;
    session.review_context = cache_base(&admission_main(&session));
    let decision = admission::scheduled(&config, &session);
    assert!(decision["primary_estimated_tokens"].is_null());
    assert_eq!(
        decision["admitted"], true,
        "an unknown economic hint must not veto review: {decision}"
    );
    assert_eq!(decision["reason"], "admitted_primary_quantity_unknown");
    assert!(
        decision["new_native_source_estimated_tokens"]
            .as_f64()
            .unwrap()
            >= 25_000.0
    );
    // A receipt from another primary route or model is still unknown here;
    // its small total must not masquerade as a known-below-floor veto.
    let main = admission_main(&session);
    for mismatched_model in [false, true] {
        let mut incompatible = session.clone();
        let mut receipt_body = primary_wire_body(&config, &main);
        if mismatched_model {
            receipt_body["model"] = json!("other-model");
        }
        incompatible.primary_input.observe(
            &receipt_body,
            &json!({"input_tokens":10}),
            if mismatched_model { &config.upstream_url } else { "other-route" },
            None,
            100,
        );
        let decision = admission::scheduled(&config, &incompatible);
        assert!(decision["primary_estimated_tokens"].is_null(), "{decision}");
        assert_eq!(decision["reason"], "admitted_primary_quantity_unknown");
    }
    session.completed_requests = 3;
    assert_eq!(admission::scheduled(&config, &session)["reason"], "cadence");
    session.completed_requests = 4;
    session.background_review_pending = true;
    assert_eq!(
        admission::scheduled(&config, &session)["reason"],
        "review_pending"
    );
    session.background_review_pending = false;
    let groups = session.groups();
    admission::cover(&mut session, &groups);
    assert_eq!(
        admission::scheduled(&config, &session)["reason"],
        "native_source_growth"
    );
    let restored: Session =
        serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
    assert_eq!(
        admission::scheduled(&config, &restored)["reason"],
        "native_source_growth"
    );
}

#[test]
fn ordinary_policy_defaults_and_explicit_bounds_are_not_legacy_aliases() {
    let config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    assert_eq!(config.review_min_primary_tokens, 0);
    assert_eq!(config.review_min_new_tokens, 25_000);
    assert_eq!(config.review_every_requests, 4);
    assert_eq!(config.review_backoff_factor, 2);
    assert_eq!(config.review_backoff_max, 8);
    for (flag, value) in [
        ("--review-every-requests", "0"),
        ("--review-min-primary-tokens", "2000001"),
        ("--review-min-new-tokens", "2000001"),
        ("--review-backoff-factor", "17"),
        ("--review-backoff-max", "65"),
    ] {
        assert!(ProxyCli::try_parse_from(["carry proxy", flag, value]).is_err());
    }
    let config = ProxyCli::try_parse_from([
        "carry proxy",
        "--review-min-primary-tokens",
        "0",
        "--review-min-new-tokens",
        "0",
        "--review-every-requests",
        "1",
        "--review-backoff-factor",
        "1",
    ])
    .unwrap();
    assert_eq!(config.review_min_primary_tokens, 0);
    assert_eq!(config.review_min_new_tokens, 0);
    assert_eq!(config.review_every_requests, 1);
    assert_eq!(admission::multiplier(&config, &admission_subject()), 1);
}

#[test]
fn opaque_payload_is_unknown_not_infinite_and_does_not_disable_visible_review_policy() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    let mut session = admission_subject();
    session.history[1].value =
        json!({"type":"reasoning","summary":[],"encrypted_content":"abCD".repeat(20000)});
    config.review_min_primary_tokens = 0;
    config.review_min_new_tokens = 0;
    let decision = admission::decision(&config, &session, &admission_main(&session), false);
    assert_eq!(decision["primary_estimated_tokens"], Value::Null);
    assert_eq!(
        decision["admitted"], true,
        "unknown native quantity must not disable a zero-floor review policy"
    );
    config.review_min_primary_tokens = 150000;
    let decision = admission::decision(&config, &session, &admission_main(&session), false);
    assert_eq!(
        decision["admitted"], true,
        "unknown primary pressure must remain an optional hint"
    );
    assert_eq!(decision["reason"], "admitted_primary_quantity_unknown");
}

#[test]
fn cadence_and_explicit_primary_quantity_do_not_eagerly_tokenize_history() {
    let config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    let mut session = admission_subject();
    session.completed_requests = 0;
    session.history[1].value["content"] = json!("large unchanged history ".repeat(10000));
    let main = admission_main(&session);
    crate::prompt_quantity::reset_tokenized_bytes();
    let decision =
        admission::decision_with_primary(&config, &session, &main, false, false, Some(200000.0));
    assert_eq!(decision["reason"], "cadence");
    assert_eq!(
        crate::prompt_quantity::tokenized_bytes(),
        0,
        "a cadence-blocked review needs no prompt or growth tokenization"
    );
}

#[test]
fn known_primary_quantity_does_not_recount_visible_history_for_diagnostics() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    config.review_min_new_tokens = 0;
    let session = admission_subject();
    crate::prompt_quantity::reset_tokenized_bytes();
    let decision = admission::decision_with_primary(
        &config,
        &session,
        &admission_main(&session),
        false,
        false,
        Some(200000.0),
    );
    assert_eq!(decision["admitted"], true);
    assert_eq!(
        crate::prompt_quantity::tokenized_bytes(),
        0,
        "provided native input and a zero growth floor need no text estimate"
    );
    assert_eq!(decision["primary_local_visible_text_tokens"], Value::Null);
}
