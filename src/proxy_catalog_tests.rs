// Synthetic frozen contracts, not vendor tariff or route evidence.
// Rates in snapshots are USD per MILLION; planner results are dollars.
use super::*;

#[test]
fn cache_write191_dispatch_frozen_lifetime_censors_only_new_written_receipts() {
    let (config, mut before, _) = fixture(Rates {
        input: 2.0,
        cached: 0.1,
        write: 2.5,
        output: 10.0,
    });
    before.cost_trajectory = Some(crate::proxy_savings::Trajectory::default());
    let mut current = before.clone();
    let reviewed = before.clone();
    let mut expected = 0.0;
    for (index, (known, written)) in [(true, 2000), (false, 2000), (false, 0), (true, 2000)]
        .into_iter()
        .enumerate()
    {
        let usage = json!({"input_tokens":2000,"output_tokens":10,
            "input_tokens_details":{"cached_tokens":100,"cache_write_tokens":written.min(1900)}});
        let model = config.classifier_model.clone();
        if known || written == 0 {
            let rates = receipt_rates(
                &model,
                &usage,
                before.model_resolution.as_ref().map(|p| &p.reviewer),
                known,
            )
            .unwrap();
            let write = written.min(1900) as f64;
            expected += (1900.0 - write) * rates.input
                + 100.0 * rates.cached
                + write * rates.write
                + 10.0 * rates.output;
        }
        let run = ReviewRun {
            receipts: vec![ReviewReceipt {
                model,
                input: 2000,
                usage: Some(usage),
                write_tariff_known: known,
                standard: true,
                completed: true,
            }],
            ..ReviewRun::default()
        };
        // A newer paid owner cannot reprice the dispatch contract or lifetime.
        current.model_resolution.as_mut().unwrap().reviewer.rates = None;
        settle_review(&mut current, &before, &reviewed, &run);
        assert_eq!(current.shadow.calls, index as u64 + 1);
        assert_eq!(current.shadow.unavailable_cost_calls, u64::from(index > 0));
        assert!((current.shadow.cost_usd - expected).abs() < 1e-12);
        let estimate = current.cost_trajectory.as_ref().unwrap().snapshot();
        assert!((estimate.observed_reviewer_cost - expected).abs() < 1e-12);
        assert_eq!(
            estimate.unavailable_reviewer_cost_calls,
            u64::from(index > 0)
        );
        current = serde_json::from_slice(&serde_json::to_vec(&current).unwrap()).unwrap();
    }
    for field in ["cache_write_input_tokens", "cache_creation_input_tokens"] {
        let usage = json!({"input_tokens":2000,"output_tokens":0,field:2000});
        assert!(receipt_rates(&config.classifier_model, &usage, None, false).is_none());
        assert!(receipt_rates(&config.classifier_model, &usage, None, true).is_some());
    }
}
// Joined acceptance: monetary lifetime censorship cannot erase #192's
// independent cache observations during real reviewer settlement and restart.
#[test]
fn joined191192_unpriced_write_settlement_keeps_independent_cache_observations() {
    let (config, mut before, _) = fixture(Rates {
        input: 2.0,
        cached: 0.1,
        write: 2.5,
        output: 10.0,
    });
    before.cost_trajectory = Some(crate::proxy_savings::Trajectory::default());
    let mut current = before.clone();
    let reviewed = before.clone();
    for (index, written) in [1900, 0].into_iter().enumerate() {
        let usage = json!({"input_tokens":2000,"output_tokens":10,
            "input_tokens_details":{"cached_tokens":100,"cache_write_tokens":written}});
        let model = config.classifier_model.clone();
        let rates = receipt_rates(
            &model,
            &usage,
            before.model_resolution.as_ref().map(|p| &p.reviewer),
            false,
        );
        assert_eq!(rates.is_none(), written > 0);
        let run = ReviewRun {
            receipts: vec![ReviewReceipt {
                model,
                input: 2000,
                usage: Some(usage),
                write_tariff_known: false,
                standard: true,
                completed: true,
            }],
            ..ReviewRun::default()
        };
        settle_review(&mut current, &before, &reviewed, &run);
        let calls = index as u64 + 1;
        assert_eq!(current.shadow.calls, calls);
        assert_eq!(current.shadow.unavailable_cost_calls, 1);
        assert_eq!(current.shadow.input_tokens, calls * 2000);
        assert_eq!(current.shadow.cached_tokens, calls * 100);
        assert_eq!(current.shadow.cache_write_tokens, 1900);
        assert_eq!(current.shadow.cache_observation.calls, calls);
        assert_eq!(current.shadow.cache_observation.input_tokens, calls * 2000);
        assert_eq!(current.shadow.cache_observation.cached_tokens, calls * 100);
        assert_eq!(current.shadow.cost_usd > 0.0, written == 0);
        let estimate = current.cost_trajectory.as_ref().unwrap().snapshot();
        assert_eq!(estimate.unavailable_reviewer_cost_calls, 1);
        assert!((estimate.observed_reviewer_cost - current.shadow.cost_usd).abs() < 1e-12);
        current = serde_json::from_slice(&serde_json::to_vec(&current).unwrap()).unwrap();
        assert_eq!(current.shadow.cache_observation.calls, calls);
        assert_eq!(current.shadow.cache_observation.input_tokens, calls * 2000);
        assert_eq!(current.shadow.cache_observation.cached_tokens, calls * 100);
    }
}

include!("proxy_projection_economics_tests.rs");

#[test]
fn reviewer_catalog_capacity_and_tier_drive_real_fit_checks_without_role_defaults() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    let route = Route::new("https://chatgpt.com/backend-api/codex/responses", true);
    let reviewer = resolve_model("gpt-6-luna", route.clone(), None, None, 2048, 0).unwrap();
    let primary = resolve_model("gpt-6-sol", route, None, None, 16384, 0).unwrap();
    config.classifier_max_input_tokens = reviewer.capacity_tokens.unwrap();
    config.context_margin_percent = 0;
    config.classifier_max_output_tokens = 2048;
    config.model_resolution = Some(ResolvedPair {
        schema_version: 1,
        primary,
        reviewer,
    });
    let body =
        json!({"model":"gpt-6-luna","input":[{"role":"user","content":"word ".repeat(350000)}]});
    let required = reviewer_required_tokens(&config, &body);
    assert!(required > 300000.0 && required < 1050000.0);
    assert!(
        review_fits(&config, &body),
        "catalog capacity, not the old reviewer role limit or cheap-tier boundary, controls feasibility"
    );
    let short = reviewer_rates(&config, 272000.0).unwrap();
    let long = reviewer_rates(&config, estimate(&body)).unwrap();
    assert_eq!(long.input, short.input * 2.0);
    assert!((long.output - short.output * 1.5).abs() < 1e-18);
    config.classifier_max_input_tokens = required.floor() as u32 - 1;
    assert!(
        !review_fits(&config, &body),
        "actual resolved capacity remains enforced"
    );
}

#[test]
fn stale_background_receipts_keep_the_dispatched_frozen_tariff_not_current_slug_or_epoch() {
    let (config, mut before, _) = fixture(Rates {
        input: 7.0,
        cached: 0.7,
        write: 9.0,
        output: 21.0,
    });
    before.cost_trajectory = Some(crate::proxy_savings::Trajectory::default());
    before.model_resolution.as_mut().unwrap().reviewer.rates = Some(Rates {
        input: 11.0,
        cached: 0.1,
        write: 15.0,
        output: 31.0,
    });
    let reviewed = before.clone();
    let mut current = before.clone();
    current.review_epoch += 1; // advice stale; the already paid receipt still counts
    current.model_resolution.as_mut().unwrap().reviewer.rates = None;
    let run = ReviewRun {
        receipts: vec![ReviewReceipt {
            model: config.classifier_model.clone(),
            input: 200,
            write_tariff_known: true,
            usage: Some(
                json!({"input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":10}}),
            ),
            standard: true,
            completed: true,
        }],
        ..ReviewRun::default()
    };
    settle_review(&mut current, &before, &reviewed, &run);
    assert_eq!(current.review_epoch, before.review_epoch + 1);
    assert_eq!(current.shadow.calls, 1);
    assert!((current.shadow.cost_usd - (90.0 * 11.0 + 10.0 * 0.1 + 31.0) / 1e6).abs() < 1e-12);
    // Advice is stale, but the paid receipt uses its native partition and
    // dispatch-frozen contract, not projected bytes or the newer missing price.
    assert!(
        (current
            .cost_trajectory
            .as_ref()
            .unwrap()
            .snapshot()
            .reviewer_cost
            - (90.0 * 11.0 + 10.0 * 0.1 + 31.0) / 1e6)
            .abs()
            < 1e-12
    );
}

#[test]
fn receipt_prediction_uses_measured_prefix_without_relaxing_capacity_reservation() {
    let (config, mut session, main) = fixture(Rates {
        input: 7.0,
        cached: 0.7,
        write: 9.0,
        output: 21.0,
    });
    let mut config = config;
    config.model_resolution = session.model_resolution.clone();
    let wire = primary_wire_body(&config, &main);
    let mut trajectory = crate::proxy_savings::Trajectory::default();
    trajectory.primary_resolved(&wire, &json!({"status":"completed","output":[],"usage":{"input_tokens":20000,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}}), &config.upstream_url, now(), true, config.model_resolution.as_ref().map(|p| &p.primary));
    session.cost_trajectory = Some(trajectory);
    let (_, report) = plan(&config, &session, &main);
    assert_eq!(
        report["primary_receipt_prediction_tokens"], 20000,
        "planning must distinguish measured history from estimates of new content"
    );
    assert!(
        report["primary_required_tokens"].as_f64().unwrap()
            >= primary_required_tokens(&config, &main),
        "receipt predictions cannot weaken existing capacity guards"
    );
}

#[test]
fn frozen_unknown_prices_do_not_fall_back_to_the_known_slug() {
    let (mut config, mut session, main) = fixture(Rates {
        input: 7.0,
        cached: 0.7,
        write: 9.0,
        output: 21.0,
    });
    session.model_resolution.as_mut().unwrap().primary.rates = None;
    config.model_resolution = session.model_resolution.clone();
    let (_, report) = plan(&config, &session, &main);
    assert_eq!(report["reason"], "unsupported_model_or_tier");
    assert!(
        report["candidates"][0]["selection_joint"].is_null(),
        "unknown price is not zero or legacy slug price"
    );
}

#[test]
fn each_candidate_prices_its_own_frozen_input_tier_after_restart() {
    let (mut config, mut session, main) = fixture(Rates {
        input: 7.0,
        cached: 0.7,
        write: 9.0,
        output: 21.0,
    });
    let groups = session.groups();
    let mut compact_body = main.clone();
    compact_body["input"] = json!(session.render_projection(&[groups[1].id].into_iter().collect()));
    let keep_tokens = estimate(&primary_wire_body(&config, &main));
    let compact_tokens = estimate(&primary_wire_body(&config, &compact_body));
    assert!(compact_tokens < keep_tokens);
    let threshold = ((keep_tokens + compact_tokens) / 2.0) as u32;
    let primary = &mut session.model_resolution.as_mut().unwrap().primary;
    primary.cheap_input_boundary = Some(threshold);
    primary.long_input_multiplier = Some(3.0);
    primary.long_output_multiplier = Some(2.0);
    config.model_resolution = session.model_resolution.clone();
    let record = semantic::Record::capture(&session);
    let mut restored: Session =
        serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
    record.apply(&mut restored);
    let (_, report) = plan(&config, &restored, &main);
    let joint = &report["candidates"][0]["selection_joint"];
    let keep = joint["primary_keep"].as_f64().unwrap();
    let compact = joint["primary_compact"].as_f64().unwrap();
    assert!((keep - keep_tokens * 7.0 * 3.0 / 1e6 * 3.0).abs() < 1e-10);
    assert!((compact - compact_tokens * 7.0 / 1e6 * 3.0).abs() < 1e-10);
}

fn fixture(rates: Rates) -> (ProxyCli, Session, Value) {
    let mut config = ProxyCli::try_parse_from([
        "carry proxy",
        "--upstream-url",
        "https://synthetic-gateway.invalid/v1/responses",
        "--classifier-model",
        "gpt-6-sol",
        "--classifier-cache-policy",
        "disabled",
        "--payoff-requests",
        "3",
        "--review-every-requests",
        "100",
        "--mode",
        "compact",
    ])
    .unwrap();
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"synthetic retained source ".repeat(800)}),
            json!({"role":"assistant","content":"synthetic disposable source ".repeat(200)}),
            json!({"role":"user","content":"finish"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    let groups = session.groups();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[1],"removable":[2],"memories":[]}),
        )
        .unwrap();
    let route = Route::new(&config.upstream_url, false);
    let mut primary = resolve_model("gpt-6-sol", route.clone(), None, None, 16384, 10).unwrap();
    primary.rates = Some(rates);
    primary.price_basis = "synthetic_fixture_only".into();
    let reviewer = resolve_model("gpt-6-sol", route, None, None, 4096, 10).unwrap();
    session.model_resolution = Some(ResolvedPair {
        schema_version: 1,
        primary,
        reviewer,
    });
    session.completed_requests = 1;
    let main = json!({"model":"gpt-6-sol","input":session.render_primary(),"store":false});
    session.review_context = cache_base(&main);
    config.model_resolution = session.model_resolution.clone();
    (config, session, main)
}

#[test]
fn planner_consumes_frozen_primary_rates_and_does_not_invent_gateway_cache_support() {
    let modeled = Rates {
        input: 7.0,
        cached: 0.0000007,
        write: 0.000009,
        output: 0.000021,
    };
    let (config, session, main) = fixture(modeled);
    let (_, report) = plan(&config, &session, &main);
    let primary_tokens = estimate(&primary_wire_body(&config, &main));
    assert!(primary_tokens > 1024.0);
    let expected = primary_tokens * modeled.input / 1e6 * 3.0;
    let actual = report["candidates"][0]["selection_joint"]["primary_keep"]
        .as_f64()
        .unwrap();
    assert!(
        (actual - expected).abs() < 1e-10,
        "frozen route has no cache capability: actual={actual} expected={expected}"
    );
    assert_eq!(
        session
            .model_resolution
            .as_ref()
            .unwrap()
            .primary
            .price_basis,
        "synthetic_fixture_only"
    );
}

#[test]
fn planner_consumes_independent_frozen_reviewer_rates_without_gateway_cache_inheritance() {
    let primary = Rates {
        input: 7.0,
        cached: 0.0000007,
        write: 0.000009,
        output: 0.000021,
    };
    let reviewer = Rates {
        input: 0.000011,
        cached: 0.0000001,
        write: 0.000015,
        output: 31.0,
    };
    let (mut config, mut session, main) = fixture(primary);
    config.review_min_primary_tokens = 0;
    config.review_min_new_tokens = 0;
    config.review_every_requests = 1;
    session.model_resolution.as_mut().unwrap().reviewer.rates = Some(reviewer);
    session
        .model_resolution
        .as_mut()
        .unwrap()
        .reviewer
        .price_basis = "synthetic_fixture_only".into();
    config.model_resolution = session.model_resolution.clone();
    let (_, report) = plan(&config, &session, &main);
    assert_eq!(
        admission::decision(&config, &session, &main, false)["admitted"],
        true
    );
    let body = review_body(&config, &session);
    let expected = (estimate(&body) * reviewer.input
        + config.classifier_max_output_tokens as f64 * reviewer.output)
        / 1e6;
    let actual = report["candidates"][0]["selection_joint"]["future_shadow_keep"]
        .as_f64()
        .unwrap();
    assert!(
        (actual - expected).abs() < 1e-10,
        "reviewer has its own frozen rates, no speculative route cache: actual={actual} expected={expected}"
    );
}

#[test]
fn primary_output_cap_reverses_warm_keep_vs_cold_tier_rewrite_with_zero_missing_unknown_controls() {
    let (mut config, mut session, mut body) = fixture(Rates {
        input: 7.0,
        cached: 0.001,
        write: 9.0,
        output: 21.0,
    });
    config.payoff_requests = 1;
    let groups = session.groups();
    let mut compact = body.clone();
    compact["input"] = json!(session.render_projection(&HashSet::from([groups[1].id])));
    let keep_tokens = estimate(&primary_wire_body(&config, &body));
    let drop_tokens = estimate(&primary_wire_body(&config, &compact));
    let primary = &mut session.model_resolution.as_mut().unwrap().primary;
    primary.cache = carry::models::model_spec("gpt-6-sol").unwrap().cache;
    primary.cheap_input_boundary = Some(((keep_tokens + drop_tokens) / 2.0) as u32);
    primary.long_input_multiplier = Some(1.0);
    primary.long_output_multiplier = Some(2.0);
    config.model_resolution = session.model_resolution.clone();
    for cap in [None, Some(0), Some(10000)] {
        body.as_object_mut().unwrap().remove("max_output_tokens");
        if let Some(cap) = cap {
            body["max_output_tokens"] = json!(cap);
        }
        session.upstream_cache.clear();
        crate::proxy_cache::observe(
            &mut session.upstream_cache,
            &primary_wire_body(&config, &body),
            &json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":10000}}),
            config.model_resolution.as_ref().map(|p| &p.primary),
            &config.primary_cache_auth,
            crate::proxy_cache::ObservationTime::during(now(), now()),
        );
        assert_eq!(session.upstream_cache.len(), 1);
        let (selected, report) = plan(&config, &session, &body);
        assert_eq!(selected.is_some(), cap == Some(10000), "{report}");
        let joint = &report["candidates"][0]["selection_joint"];
        if cap == Some(10000) {
            assert!(
                joint["primary_compact"].as_f64().unwrap()
                    < joint["primary_keep"].as_f64().unwrap()
            );
        } else {
            assert!(
                joint["primary_keep"].as_f64().unwrap()
                    < joint["primary_compact"].as_f64().unwrap()
            );
        }
    }
    config
        .model_resolution
        .as_mut()
        .unwrap()
        .primary
        .long_output_multiplier = Some(1.0);
    assert!(
        plan(&config, &session, &body).0.is_none(),
        "same output rate cannot reverse input ranking"
    );
    config.model_resolution.as_mut().unwrap().primary.rates = None;
    let (selected, report) = plan(&config, &session, &body);
    assert!(selected.is_none());
    assert_eq!(report["reason"], "unsupported_model_or_tier");
    assert!(report["candidates"][0]["selection_joint"].is_null());
}
