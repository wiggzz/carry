// PR182 acceptance intent on immutable history: no standalone reviewer rebuild.
// Zero-priced synthetic primary isolates the real frozen review tariff; it is
// not a free-primary production policy. All removals still require actual advice.
fn projection_economics_fixture() -> (ProxyCli, Session, Value) {
    let (mut config, mut session, main) = fixture(Rates {
        input: 0.0,
        cached: 0.0,
        write: 0.0,
        output: 0.0,
    });
    config.payoff_requests = 1;
    config.min_payback_percent = 0;
    config.context_margin_percent = 0;
    config.primary_context_tokens = 1_000_000;
    config.classifier_max_input_tokens = 1_000_000;
    config.review_min_primary_tokens = 0;
    config.review_min_new_tokens = 0;
    config.review_every_requests = 1;
    session.model_resolution.as_mut().unwrap().reviewer = resolve_model(
        &config.classifier_model,
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        config.classifier_max_output_tokens,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    session.apply_advice(&session.groups(), &json!({"protected":[1],"removable":[2],"memories":[{"source_ids":[2],"text":"retained accepted fact"}]})).unwrap();
    assert_eq!(session.memories.len(), 1);
    config.model_resolution = session.model_resolution.clone();
    assert_eq!(admission::scheduled(&config, &session)["admitted"], true);
    (config, session, main)
}

fn projection_economics_joint(report: &Value) -> &Value {
    let rows = report["candidates"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{report}");
    assert_eq!(rows[0]["action"], "projection");
    assert_eq!(rows[0]["omitted_members"], json!([2]));
    &rows[0]["selection_joint"]
}

fn projection_economics_close(actual: &Value, expected: f64) {
    let actual = actual.as_f64().unwrap();
    assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
}

#[test]
fn projection182_actual_reviewer_tier_crossing_prices_input_and_output() {
    let (config, mut session, mut main) = projection_economics_fixture();
    session.history[1].value["content"] = json!("word ".repeat(300_000));
    main["input"] = json!(session.render_primary());
    let canonical = session.render_primary();
    let (selected, report) = plan(&config, &session, &main);
    let projection = selected.expect("cold, due reviewer savings justify authorized projection");
    let keep = review_body(&config, &session);
    let compact = review_body_projected(&config, &session, &projection);
    let (kt, ct) = (estimate(&keep), estimate(&compact));
    assert!(kt > 272_000.0 && ct < 272_000.0, "{kt}/{ct}");
    assert!(review_fits(&config, &keep) && review_fits(&config, &compact));
    let kr = reviewer_rates(&config, kt).unwrap();
    let cr = reviewer_rates(&config, ct).unwrap();
    assert!(kr.input > cr.input && kr.output > cr.output);
    let output = f64::from(config.classifier_max_output_tokens);
    let joint = projection_economics_joint(&report);
    projection_economics_close(
        &joint["future_shadow_keep"],
        kt * kr.input + output * kr.output,
    );
    projection_economics_close(
        &joint["future_shadow_compact"],
        ct * cr.input + output * cr.output,
    );
    assert_eq!(report["selected_action"], "projection");
    assert_eq!(projection.omitted, HashSet::from([2]));
    assert!(projection.input.contains(&canonical[0]) && projection.input.contains(&canonical[2]));
    assert!(!projection.input.contains(&canonical[1]));
    assert_eq!(
        session.render_primary(),
        canonical,
        "selection cannot edit H"
    );
}

#[test]
fn projection182_warm_reviewer_receipt_veto_expires_without_erasing_advice() {
    let (config, mut session, main) = projection_economics_fixture();
    assert!(plan(&config, &session, &main).0.is_some());
    let keep = review_body(&config, &session);
    assert!(crate::proxy_cache::observe(
        &mut session.upstream_cache,
        &keep,
        &json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":10000}}),
        config.model_resolution.as_ref().map(|p| &p.reviewer),
        &config.reviewer_cache_auth,
        crate::proxy_cache::ObservationTime::during(now(), now()),
    ));
    let canonical = session.render_primary();
    let opinions = session.opinions.clone();
    let drops = session.drop_members.clone();
    let memories = serde_json::to_value(&session.memories).unwrap();
    let cache = serde_json::to_value(&session.upstream_cache).unwrap();
    let compactions = session.compactions;
    let (selected, report) = plan(&config, &session, &main);
    assert!(
        selected.is_none(),
        "warm keep beats cold projected review: {report}"
    );
    let joint = projection_economics_joint(&report);
    assert!(
        joint["future_shadow_keep"].as_f64().unwrap()
            < joint["future_shadow_compact"].as_f64().unwrap()
    );
    assert_eq!(
        serde_json::to_value(&session.upstream_cache).unwrap(),
        cache
    );
    // Age the genuine receipt, not a generic warm flag or semantic authority.
    for receipt in &mut session.upstream_cache {
        let window = receipt
            .write_window
            .as_mut()
            .expect("timed whole-prefix write");
        window.not_before = now().saturating_sub(window.lifetime + 1);
        window.not_after = window.not_before;
    }
    assert!(plan(&config, &session, &main).0.is_some());
    assert_eq!(session.render_primary(), canonical);
    assert_eq!(session.opinions, opinions);
    assert_eq!(session.drop_members, drops);
    assert_eq!(serde_json::to_value(&session.memories).unwrap(), memories);
    assert_eq!(session.compactions, compactions);
}

#[test]
fn projection182_reviewer_output_allowance_moves_strict_payback() {
    let (mut config, session, main) = projection_economics_fixture();
    let mut ratios = Vec::new();
    for output in [1, 16_384] {
        config.classifier_max_output_tokens = output;
        let (_, report) = plan(&config, &session, &main);
        let joint = projection_economics_joint(&report);
        assert!(joint["savings"].as_f64().unwrap() > 0.0);
        ratios.push(100.0 * joint["savings"].as_f64().unwrap() / joint["keep"].as_f64().unwrap());
    }
    let margin = ((ratios[0] + ratios[1]) / 2.0).floor() as u8;
    assert!(ratios[1] < f64::from(margin) && f64::from(margin) < ratios[0]);
    config.min_payback_percent = margin;
    for (output, accepted) in [(1, true), (16_384, false)] {
        config.classifier_max_output_tokens = output;
        let (selected, report) = plan(&config, &session, &main);
        let joint = projection_economics_joint(&report);
        assert_eq!(selected.is_some(), accepted, "{report}");
        assert_eq!(joint["accepted"], accepted);
        assert_eq!(
            joint["savings"].as_f64().unwrap()
                > joint["keep"].as_f64().unwrap() * f64::from(margin) / 100.0,
            accepted
        );
    }
}

#[tokio::test]
async fn projection182_context_fit_forces_only_authorized_projection_not_reviewer_cleanup() {
    let (mut config, session, main) = projection_economics_fixture();
    config.min_payback_percent = 100;
    assert!(plan(&config, &session, &main).0.is_none());
    let omitted = HashSet::from([2]);
    let mut compact = main.clone();
    compact["input"] = json!(session.render_projection(&omitted));
    config.primary_context_tokens = primary_required_tokens(&config, &compact).ceil() as u32;
    let projection = planning::Projection {
        input: session.render_projection(&omitted),
        omitted,
    };
    config.classifier_max_input_tokens = reviewer_required_tokens(
        &config,
        &review_body_projected(&config, &session, &projection),
    )
    .ceil() as u32;
    assert!(!primary_fits(&config, &main) && primary_fits(&config, &compact));
    assert!(!review_fits(&config, &review_body(&config, &session)));
    assert!(review_fits(
        &config,
        &review_body_projected(&config, &session, &projection)
    ));
    config.model_resolution.as_mut().unwrap().primary.rates = None;
    let (selected, report) = plan(&config, &session, &main);
    assert_eq!(report["forced"], true);
    assert!(projection_economics_joint(&report).is_null());
    assert_eq!(selected.unwrap().omitted, projection.omitted);
    // Real dispatcher rejects an oversized canonical review; it may not clear
    // semantic state or resurrect the retired standalone cleanup machinery.
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    config.state_dir = directory.path().to_path_buf();
    // Even a broken fit guard must never reach a real provider from this test.
    config.classifier_url = Some("http://127.0.0.1:9/review".into());
    service.config = config;
    let service = Arc::new(service);
    let mut after = session.clone();
    let before = serde_json::to_value(&after).unwrap();
    dispatch_review_for(
        service.clone(),
        "projection182-no-fit".into(),
        &mut after,
        planning::Projection::canonical(&session),
        "synthetic".into(),
    )
    .await;
    assert!(service.background_reviews.lock().await.is_empty());
    assert_eq!(serde_json::to_value(&after).unwrap(), before);
    assert_eq!(after.opinions, session.opinions);
    assert_eq!(after.drop_members, session.drop_members);
    assert_eq!(
        serde_json::to_value(&after.memories).unwrap(),
        serde_json::to_value(&session.memories).unwrap()
    );
    assert_eq!(after.render_primary(), session.render_primary());
}

#[test]
fn projection182_absent_pending_schedules_and_completed_spend_cannot_subsidize_selection() {
    let (mut config, session, main) = projection_economics_fixture();
    let (selected, original) = plan(&config, &session, &main);
    assert!(selected.is_some());
    let mut paid = session.clone();
    paid.shadow.observe(&config.classifier_model, &json!({"input_tokens":100000,"output_tokens":20000,"input_tokens_details":{"cached_tokens":0}}), true);
    assert!(paid.shadow.cost_usd > session.shadow.cost_usd);
    assert_eq!(
        plan(&config, &paid, &main).1,
        original,
        "completed spend is sunk"
    );
    for pending in [false, true] {
        config.review_every_requests = if pending { 1 } else { 100 };
        let mut unavailable = paid.clone();
        unavailable.background_review_pending = pending;
        assert_eq!(
            admission::scheduled(&config, &unavailable)["admitted"],
            false
        );
        let (selected, report) = plan(&config, &unavailable, &main);
        assert!(selected.is_none(), "no future review subsidy: {report}");
        let joint = projection_economics_joint(&report);
        assert_eq!(joint["future_shadow_keep"], 0.0);
        assert_eq!(joint["future_shadow_compact"], 0.0);
        assert_eq!(joint["savings"], 0.0);
        assert_eq!(
            joint["accepted"], false,
            "strict zero-payback rejects a tie"
        );
    }
}

// Additional acceptance for the HTTP-reproduced missing marginal baseline.
// Staged last-submitted IDs identify a wire, never staged drop authority.
fn marginal_projection_fixture() -> (ProxyCli, Session, Value) {
    let (mut config, mut session, mut body) = fixture(Rates {
        input: 1.0,
        cached: 0.01,
        write: 1.0,
        output: 0.0,
    });
    config.payoff_requests = 1;
    config.context_margin_percent = 0;
    config.primary_context_tokens = 1_000_000;
    config.min_payback_percent = 0;
    session.last_submitted_ids = vec![1, 3];
    let mut input = session.render_primary();
    input.extend([
        json!({"role":"assistant","content":"new disposable details ".repeat(50)}),
        json!({"role":"user","content":"next task"}),
    ]);
    session.ingest(&input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":[4],"memories":[]}),
        )
        .unwrap();
    body["input"] = json!(input);
    (config, session, body)
}

#[test]
fn marginal_continuation_revalidates_advice_and_renders_current_memory_anchors() {
    let (mut config, mut session, body) = marginal_projection_fixture();
    config.min_payback_percent = 100;
    let canonical = session.render_primary();
    let prior_ids = session.last_submitted_ids.clone();
    session.apply_advice(&session.groups(), &json!({"protected":[],"removable":[],"memories":[{"source_ids":[2],"text":"CURRENT_FACT"}]})).unwrap();
    let (selected, report) = plan(&config, &session, &body);
    let projection = selected.unwrap();
    assert_eq!(report["baseline_omitted_members"], json!([2]));
    assert_eq!(projection.omitted, HashSet::from([2]));
    assert_eq!(projection.input[0], canonical[0]);
    assert!(!projection.input.contains(&canonical[1]));
    assert!(
        projection.input[2]["content"]
            .as_str()
            .unwrap()
            .contains("CURRENT_FACT")
    );
    assert_eq!(&projection.input[3..], &canonical[2..]);
    assert_eq!(session.render_primary(), canonical);
    assert_eq!(
        session.last_submitted_ids, prior_ids,
        "planning is read-only"
    );
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[2],"removable":[],"memories":[]}),
        )
        .unwrap();
    let (selected, report) = plan(&config, &session, &body);
    assert_eq!(report["baseline_omitted_members"], json!([]));
    assert!(
        selected.is_none(),
        "past omission cannot override current protection: {report}"
    );
}

#[test]
fn marginal_capacity_uses_fitting_continuation_before_forcing_another_edit() {
    let (mut config, session, body) = marginal_projection_fixture();
    config.min_payback_percent = 100;
    let required = |ids: HashSet<u64>| {
        let mut view = body.clone();
        view["input"] = json!(session.render_projection(&ids));
        primary_required_tokens(&config, &view)
    };
    let prior = required(HashSet::from([2]));
    let rewritten = required(HashSet::from([2, 4]));
    let canonical = required(HashSet::new());
    assert!(rewritten < prior && prior < canonical);
    config.primary_context_tokens = prior.ceil() as u32;
    let (selected, report) = plan(&config, &session, &body);
    assert_eq!(
        report["forced"], false,
        "canonical overflow is not marginal pressure"
    );
    assert_eq!(selected.unwrap().omitted, HashSet::from([2]));
    config.primary_context_tokens = rewritten.ceil() as u32;
    config.model_resolution.as_mut().unwrap().primary.rates = None;
    let (selected, report) = plan(&config, &session, &body);
    assert_eq!(report["forced"], true);
    assert_eq!(selected.unwrap().omitted, HashSet::from([2, 4]));
    config.primary_context_tokens = 1;
    let (selected, report) = plan(&config, &session, &body);
    assert_eq!(report["forced"], true);
    assert!(
        selected.is_none(),
        "no authorized alternative fits: {report}"
    );
}

#[test]
fn marginal_warm_veto_requires_current_scoped_cache_evidence_not_omission_history() {
    let (mut config, mut session, body) = marginal_projection_fixture();
    let model = &mut config.model_resolution.as_mut().unwrap().primary;
    model.cache = resolve_model(
        "gpt-6-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        16384,
        10,
    )
    .unwrap()
    .cache;
    model.scenario_cache_policy = model
        .clone()
        .with_scenario_cache_policy()
        .scenario_cache_policy;
    let mut prior = body.clone();
    prior["input"] = json!(session.render_projection(&HashSet::from([2])));
    let prior = primary_wire_body(&config, &prior);
    assert!(crate::proxy_cache::observe(
        &mut session.upstream_cache,
        &prior,
        &json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":10000}}),
        config.model_resolution.as_ref().map(|p| &p.primary),
        &config.primary_cache_auth,
        crate::proxy_cache::ObservationTime::during(now(), now())
    ));
    let (selected, report) = plan(&config, &session, &body);
    assert_eq!(
        selected.unwrap().omitted,
        HashSet::from([2]),
        "warm continuation wins: {report}"
    );
    assert!(report["primary_cached_estimated_tokens"].as_f64().unwrap() > 0.0);
    for control in ["cold", "expired", "unknown", "route", "credential"] {
        let mut cold = session.clone();
        let mut config = config.clone();
        match control {
            "cold" => cold.upstream_cache.clear(),
            "expired" => {
                for entry in &mut cold.upstream_cache {
                    let window = entry
                        .write_window
                        .as_mut()
                        .expect("timed whole-prefix write");
                    window.not_before = now().saturating_sub(window.lifetime + 1);
                    window.not_after = window.not_before;
                }
            }
            "unknown" => config.model_resolution.as_mut().unwrap().primary.cache = None,
            "route" => {
                config.model_resolution.as_mut().unwrap().primary.route =
                    Route::new("https://other.invalid/v1/responses", false)
            }
            "credential" => config.primary_cache_auth = "different-fixture-key-digest".into(),
            _ => unreachable!(),
        }
        let (selected, report) = plan(&config, &cold, &body);
        assert_eq!(report["primary_cached_estimated_tokens"], 0.0, "{control}");
        assert_eq!(
            selected.unwrap().omitted,
            HashSet::from([2, 4]),
            "profitable cold rewrite {control}: {report}"
        );
    }
}
