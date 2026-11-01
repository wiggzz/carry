// Test-only wall clock seam: each synchronous decision stays on this thread.
// Advancing the clock models work crossing an original write's expiry, not a
// real vendor residency claim. Production still uses the ordinary system clock.
thread_local! {
    static DECISION_CLOCK: std::cell::RefCell<Option<std::collections::VecDeque<u64>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn decision_clock_now() -> Option<u64> {
    DECISION_CLOCK.with_borrow_mut(|clock| {
        let values = clock.as_mut()?;
        if values.len() > 1 {
            values.pop_front()
        } else {
            values.front().copied()
        }
    })
}

fn at_decision_clock<T>(times: &[u64], operation: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            DECISION_CLOCK.with_borrow_mut(|clock| *clock = None);
        }
    }
    DECISION_CLOCK.with_borrow_mut(|clock| {
        assert!(clock.is_none(), "clock fixtures cannot nest");
        assert!(!times.is_empty());
        *clock = Some(times.iter().copied().collect());
    });
    let _reset = Reset;
    operation()
}

fn clock_subject() -> (ProxyCli, Session, Value, u64) {
    let mut config = reviewer_config();
    config.payoff_requests = 1;
    config.min_payback_percent = 0;
    config.review_min_primary_tokens = u32::MAX;
    let model = resolve_model(
        "gpt-6-luna",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        2048,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    let expiry = 100 + model.scenario_cache_policy.as_ref().unwrap().ttl_seconds;
    config.model_resolution = Some(carry::models::ResolvedPair {
        schema_version: 1,
        primary: model.clone(),
        reviewer: model.clone(),
    });
    let mut session = semantic_subject();
    session.history[0].value["content"] = json!("protected warm direction ".repeat(6000));
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":[2],"memories":[]}),
        )
        .unwrap();
    let body = json!({"model":"gpt-6-luna","input":session.render_primary()});
    let wire = primary_wire_body(&config, &body);
    assert!(crate::proxy_cache::observe(
        &mut session.upstream_cache,
        &wire,
        &json!({"input_tokens":50000,"cached_input_tokens":0,"cache_write_input_tokens":50000}),
        Some(&model),
        &config.primary_cache_auth,
        crate::proxy_cache::ObservationTime::during(100, 100),
    ));
    assert!(
        crate::proxy_cache::reusable(
            &session.upstream_cache,
            &wire,
            Some(&model),
            &config.primary_cache_auth,
            expiry - 1
        ) > 0.0
    );
    assert_eq!(
        crate::proxy_cache::reusable(
            &session.upstream_cache,
            &wire,
            Some(&model),
            &config.primary_cache_auth,
            expiry
        ),
        0.0
    );
    (config, session, body, expiry)
}

#[test]
fn identical_views_cannot_gain_or_lose_money_between_clock_reads() {
    let (config, session, body, expiry) = clock_subject();
    let model = &config.model_resolution.as_ref().unwrap().primary;
    let wire = primary_wire_body(&config, &body);
    let price = || {
        resolved_view_costs(
            &wire,
            &wire,
            &session.upstream_cache,
            "gpt-6-luna",
            1,
            Some(model),
            &config.primary_cache_auth,
            now(),
        )
        .unwrap()
    };
    let warm = at_decision_clock(&[expiry - 1], price);
    let cold = at_decision_clock(&[expiry], price);
    assert_eq!(warm.keep, warm.compact);
    assert_eq!(cold.keep, cold.compact);
    assert!(
        warm.keep < cold.keep,
        "qualified write still has economic effect"
    );
    let crossing = at_decision_clock(&[expiry - 1, expiry], price);
    assert_eq!(
        crossing.keep, crossing.compact,
        "DECISION_CLOCK_SYMMETRY: identical outgoing alternatives need the same eligibility instant"
    );
}

#[test]
fn planner_selection_and_cache_report_share_one_original_write_clock() {
    let (config, session, body, expiry) = clock_subject();
    let before = serde_json::to_vec(&session).unwrap();
    let run = || plan(&config, &session, &body);
    let (warm_projection, warm) = at_decision_clock(&[expiry - 1], run);
    let (cold_projection, cold) = at_decision_clock(&[expiry], run);
    assert!(warm_projection.is_none(), "warm rewrite loses: {warm}");
    assert!(cold_projection.is_some(), "cold rewrite pays: {cold}");
    assert!(warm["primary_cached_estimated_tokens"].as_f64().unwrap() > 0.0);
    assert_eq!(cold["primary_cached_estimated_tokens"], 0.0);
    let (crossing_projection, crossing) = at_decision_clock(&[expiry - 1, expiry], run);
    assert_eq!(
        crossing_projection.map(|p| p.omitted),
        warm_projection.map(|p| p.omitted),
        "DECISION_CLOCK_SELECTION: later clock reads cannot change the frozen decision: {crossing}"
    );
    assert_eq!(
        crossing, warm,
        "DECISION_CLOCK_REPORT: enumeration, alternatives and disclosure must share eligibility"
    );
    assert_eq!(
        serde_json::to_vec(&session).unwrap(),
        before,
        "eligibility reads cannot change receipts, advice, history or money"
    );
}
