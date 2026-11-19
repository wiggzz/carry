// Test-only wall clock seam: each synchronous decision stays on this thread.
// Advancing the clock models work crossing an original write's expiry, not a
// real vendor residency claim. Production still uses the ordinary system clock.
thread_local! {
    static DECISION_CLOCK: std::cell::RefCell<Option<std::collections::VecDeque<u64>>> = const { std::cell::RefCell::new(None) };
    static CACHE_FIXTURE_ROUTE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn attest_loopback_cache_fixture(url: &str, route: &mut Route) {
    CACHE_FIXTURE_ROUTE.with_borrow(|fixture| {
        if fixture.as_deref() == Some(url) {
            route.class = RouteClass::OfficialApi;
        }
    });
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

#[test]
fn payoff_horizon_exposes_both_spacing_bounds_for_both_wires() {
    use crate::proxy_quantity::{EstimateBasis, InputEstimate};
    let (config, session, keep, expiry) = clock_subject();
    let mut compact = keep.clone();
    compact["input"] = json!(session.render_projection(&HashSet::from([2])));
    let model = &config.model_resolution.as_ref().unwrap().primary;
    let forecast = |at, model| {
        resolved_input_costs(&keep, &compact, &session.upstream_cache, "gpt-6-luna", 4,
            Some(model), &config.primary_cache_auth, at,
            InputEstimate { tokens: 50_000, basis: EstimateBasis::ProviderReceipt },
            InputEstimate { tokens: 44_000, basis: EstimateBasis::ReceiptAnchoredTextDelta }).unwrap()
    };
    let warm = forecast(expiry - 1, model);
    let cold = forecast(expiry, model);
    assert!(warm.keep < cold.keep);
    assert_eq!(warm.compact, cold.compact);
    for branch in [&cold.keep_cache, &cold.compact_cache] {
        assert_eq!(branch.all_cold_usd, branch.current_evidence_future_cold_usd);
        assert!(branch.current_evidence_future_warm_usd.unwrap() < branch.all_cold_usd);
    }
    let mut gateway = model.clone();
    gateway.route = Route::new("https://gateway.invalid/v1/responses", false);
    let unknown = forecast(expiry - 1, &gateway);
    for branch in [unknown.keep_cache, unknown.compact_cache] {
        assert_eq!(branch.first_supported_cached_tokens, 0.0);
        assert!(branch.current_evidence_future_warm_usd.is_none());
    }
}

#[test]
fn implicit_write_tariff_changes_cold_horizon_cost_order() {
    use crate::proxy_quantity::{EstimateBasis, InputEstimate};
    let (config, session, original, _) = clock_subject();
    let model = &config.model_resolution.as_ref().unwrap().primary;
    for (ttl, write_rate) in [("30m", 0.125), ("12h", 0.25)] {
        let mut keep = original.clone();
        keep["prompt_cache_options"] = json!({"ttl": ttl});
        let mut compact = keep.clone();
        compact["input"] = json!(session.render_projection(&HashSet::from([2])));
        let mut cache = Vec::new();
        assert!(crate::proxy_cache::observe(
            &mut cache,
            &keep,
            &json!({"input_tokens":50000,"cached_input_tokens":0,"cache_write_input_tokens":50000}),
            Some(model),
            &config.primary_cache_auth,
            crate::proxy_cache::ObservationTime::during(100, 100),
        ));
        let forecast = resolved_input_costs(
            &keep,
            &compact,
            &cache,
            "gpt-6-luna",
            4,
            Some(model),
            &config.primary_cache_auth,
            101,
            InputEstimate {
                tokens: 50_000,
                basis: EstimateBasis::ProviderReceipt,
            },
            InputEstimate {
                tokens: 38_600,
                basis: EstimateBasis::ReceiptAnchoredTextDelta,
            },
        )
        .unwrap();
        assert_eq!(forecast.keep_cache.first_supported_cached_tokens, 50_000.0);
        assert_eq!(forecast.compact_cache.first_supported_cached_tokens, 0.0);
        let expected_keep = (50_000.0 * 0.01 + 3.0 * 50_000.0 * write_rate) / 1e6;
        let expected_compact = 4.0 * 38_600.0 * write_rate / 1e6;
        assert!(
            forecast.keep < forecast.compact,
            "cache-write tariff changes the selected cost order for {ttl}: {forecast:?}"
        );
        assert!((forecast.keep - expected_keep).abs() < 1e-12);
        assert!((forecast.compact - expected_compact).abs() < 1e-12);
    }
}

#[tokio::test]
async fn documented_long_retention_changes_http_wire_at_boundary_after_restart() {
    use std::collections::VecDeque;

    let (capture, mut wires) = tokio::sync::mpsc::unbounded_channel();
    let endpoint = Router::new().route("/primary", post(move |axum::Json(body): axum::Json<Value>| {
        let first = capture.send(body.clone()).is_ok() && body["input"].as_array().is_some_and(|items| items.len() == 3);
        async move {
            let mut response = json!({"id":"cache-boundary","status":"completed","output":[],
                "usage":{"input_tokens":50000,"output_tokens":0}});
            if first {
                response["usage"] = json!({"input_tokens":50000,"output_tokens":0,
                    "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":50000}});
            }
            axum::Json(response)
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/primary", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, endpoint).await.unwrap() });
    CACHE_FIXTURE_ROUTE.with_borrow_mut(|route| *route = Some(url.clone()));
    let directory = tempfile::tempdir().unwrap();
    let start_service = || {
        let mut service = checkpoint_service(directory.path());
        service.config.upstream_url = url.clone();
        service.config.classifier_url = Some(url.clone());
        service.config.classifier_cache_policy = ClassifierCachePolicy::Disabled;
        service.config.mode = Mode::Compact;
        service.config.payoff_requests = 1;
        service.config.min_payback_percent = 0;
        service.config.review_min_primary_tokens = u32::MAX;
        service.config.bootstrap_min_input_bytes = usize::MAX;
        Arc::new(service)
    };
    let (_, session, _, _) = clock_subject();
    let input = session.render_primary();
    let body = json!({"model":"gpt-6-luna","prompt_cache_options":{"ttl":"12h"}, "input":input});
    let send = |service: Arc<Service>, key: &'static str, at: u64, body: Value| async move {
        DECISION_CLOCK.with_borrow_mut(|clock| *clock = Some(VecDeque::from([at])));
        let mut headers = HeaderMap::new();
        headers.insert("x-carry-session", key.parse().unwrap());
        let id = identity(&headers).unwrap().unwrap();
        let result = forward_request(service.clone(), headers, Bytes::from(body.to_string()), false, at.to_string()).await;
        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let plan = load(&service, &id).unwrap().last_plan;
        DECISION_CLOCK.with_borrow_mut(|clock| *clock = None);
        plan
    };
    let service = start_service();
    let start = 5_000_000;
    for key in ["authorized", "protected", "uneconomic"] {
        send(service.clone(), key, start, body.clone()).await;
        assert_eq!(wires.recv().await.unwrap()["input"], body["input"]);
        let mut headers = HeaderMap::new();
        headers.insert("x-carry-session", key.parse().unwrap());
        let id = identity(&headers).unwrap().unwrap();
        let mut persisted = load(&service, &id).unwrap();
        if key != "protected" {
            persisted.apply_advice(&persisted.groups(), &json!({"protected":[],"removable":[2],"memories":[]})).unwrap();
            at_decision_clock(&[start], || history::publish(&service, &persisted, &persisted)).unwrap();
        }
    }
    let mut followup = body.clone();
    followup["input"].as_array_mut().unwrap().push(json!({"role":"user","content":"next step"}));
    let expiry = start + 12 * 3600;
    let warm = send(service.clone(), "authorized", expiry - 1, followup.clone()).await;
    assert_eq!(warm["cache_evidence"]["state"], "supported", "{warm}");
    assert!(wires.recv().await.unwrap()["input"] == followup["input"], "qualified 12h write should preserve the warm wire before expiry: {warm}");
    drop(service);
    let service = start_service();
    send(service.clone(), "protected", expiry, followup.clone()).await;
    assert_eq!(wires.recv().await.unwrap()["input"], followup["input"]);
    for at in [expiry, expiry + 86_400] {
        let cold = send(service.clone(), "authorized", at, followup.clone()).await;
        let compact = wires.recv().await.unwrap();
        assert!(compact.to_string().len() < followup.to_string().len(), "{cold}");
        assert!(!compact.to_string().contains("large evidence"));
        assert_eq!(cold["primary_cached_estimated_tokens"], 0.0);
        if at == expiry {
            assert_eq!(cold["cache_evidence"]["state"], "expired_credit_not_proof_of_eviction");
        }
    }
    let mut uneconomic = start_service();
    Arc::get_mut(&mut uneconomic).unwrap().config.min_payback_percent = 100;
    send(uneconomic, "uneconomic", expiry, followup.clone()).await;
    assert_eq!(wires.recv().await.unwrap()["input"], followup["input"]);
    assert!(wires.try_recv().is_err(), "no expiry-triggered reviewer call");
    handle.abort();
    CACHE_FIXTURE_ROUTE.with_borrow_mut(|route| *route = None);
}
