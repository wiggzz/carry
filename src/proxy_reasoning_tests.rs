// Synthetic-only behavioral acceptance for forecast sizing and its consumers.
use super::*;

#[test]
fn reasoning_selection_uses_native_residual_not_ciphertext_size() {
    for (repeats, selected) in [(1, true), (10_000, true)] {
        let (mut config, _, _) = fixture(Rates {
            input: 2.0,
            cached: 0.2,
            write: 2.0,
            output: 0.0,
        });
        config.payoff_requests = 1;
        config.min_payback_percent = 10;
        config.context_margin_percent = 0;
        config.primary_context_tokens = 100_000;
        let mut session = Session::default();
        let main = json!({"model":"gpt-6-sol","input":[
            {"role":"user","content":"retained direction ".repeat(400)},
            {"type":"reasoning","summary":[],"encrypted_content":"synthetic".repeat(repeats)},
            {"type":"function_call","call_id":"inspect","name":"inspect","arguments":"{}"},
            {"type":"function_call_output","call_id":"inspect","output":"done"},
            {"role":"user","content":"continue"}]});
        session.ingest(main["input"].as_array().unwrap()).unwrap();
        for item in &mut session.history {
            item.exposed = true;
        }
        session
            .apply_advice(
                &session.groups(),
                &json!({"protected":[1],"removable":[2],"memories":[]}),
            )
            .unwrap();
        session.completed_requests = 1;
        session.model_resolution = config.model_resolution.clone();
        let wire = primary_wire_body(&config, &main);
        session.primary_input.observe(
            &wire,
            &json!({"input_tokens":10000}),
            &config.upstream_url,
            config.model_resolution.as_ref().map(|p| &p.primary),
            100,
        );
        let before = serde_json::to_value(&session).unwrap();
        let (projection, report) = plan(&config, &session, &main);
        assert_eq!(projection.is_some(), selected, "{report}");
        assert_eq!(report["primary_estimated_tokens"], 10000);
        let candidate = &report["candidates"][0];
        assert_eq!(
            candidate["primary_input_basis"],
            "receipt_anchored_reasoning_forecast"
        );
        let mut target = wire.clone();
        target["input"] = json!(projection.as_ref().unwrap().input);
        let quantity = session
            .primary_input
            .accounting_restoration(
                &target,
                &wire,
                &config.upstream_url,
                config.model_resolution.as_ref().map(|p| &p.primary),
            )
            .unwrap();
        assert_eq!(
            candidate["primary_retained_estimated_tokens"],
            u64::try_from(10000 + quantity.tokens).unwrap()
        );
        assert!(candidate["primary_retained_required_tokens"].is_null());
        assert_eq!(candidate["selection_joint"]["accepted"], selected);
        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            before,
            "planning cannot change receipts, money, cache or authority"
        );
        session.opinions.insert(2, "keep".into());
        assert!(
            plan(&config, &session, &main).0.is_none(),
            "receipt sizing grants no removal authority"
        );
    }
}

#[test]
fn reasoning_forecast_planner_continues_after_filtered_receipt_and_restart() {
    let (mut config, _, _) = fixture(Rates {
        input: 2.0,
        cached: 0.2,
        write: 2.0,
        output: 0.0,
    });
    config.payoff_requests = 1;
    config.min_payback_percent = 0;
    config.primary_context_tokens = 100_000;
    let mut session = Session {
        model_resolution: config.model_resolution.clone(),
        ..Session::default()
    };
    let mut input = vec![json!({"role":"user","content":"goal"})];
    for id in ["a", "b"] {
        input.extend([
            json!({"type":"reasoning","summary":[],"encrypted_content":format!("synthetic-{id}").repeat(1000)}),
            json!({"type":"function_call","call_id":id,"name":"inspect","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":id,"output":"done"}),
        ]);
    }
    input.push(json!({"role":"user","content":"current"}));
    session.ingest(&input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[1],"removable":[2],"memories":[]}),
        )
        .unwrap();
    session.completed_requests = 1;
    let mut main = json!({"model":"gpt-6-sol","input":input});
    let observe = |session: &mut Session, body: &Value, tokens, at| {
        session.primary_input.observe(
            &primary_wire_body(&config, body),
            &json!({"input_tokens":tokens}),
            &config.upstream_url,
            config.model_resolution.as_ref().map(|p| &p.primary),
            at,
        );
    };
    observe(&mut session, &main, 10000u64, 100);
    let first = plan(&config, &session, &main).0.unwrap();
    assert_eq!(first.omitted, HashSet::from([2, 3, 4]));
    session.last_submitted_ids = vec![1, 5, 6, 7, 8];
    let mut filtered = main.clone();
    filtered["input"] = json!(first.input);
    observe(&mut session, &filtered, 6000, 101);
    let mut session: Session =
        serde_json::from_value(serde_json::to_value(session).unwrap()).unwrap();
    let mut next_input = session.render_primary();
    next_input.push(json!({"role":"user","content":"fresh append"}));
    session.ingest(&next_input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session
        .apply_advice(
            &session.groups(),
            // Advice is a complete authority snapshot: retain the first
            // group's permission as well as authorizing the next group.
            &json!({"protected":[],"removable":[2,5],"memories":[]}),
        )
        .unwrap();
    main["input"] = json!(next_input);
    let frozen = serde_json::to_value(&session).unwrap();
    let (selected, report) = plan(&config, &session, &main);
    assert_eq!(
        report["baseline_omitted_members"],
        json!([2, 3, 4]),
        "{report}; submitted={:?}; groups={:?}; authority={:?}",
        session.last_submitted_ids,
        session.groups(),
        session.drop_members
    );
    assert!(report["primary_estimated_tokens"].as_u64().unwrap() >= 6000);
    assert_eq!(
        selected.unwrap().omitted,
        HashSet::from([2, 3, 4, 5, 6, 7]),
        "{report}"
    );
    assert_eq!(serde_json::to_value(&session).unwrap(), frozen);
}

#[test]
fn reasoning_forecast_does_not_assume_future_cache_writes() {
    let source = json!({"model":"gpt-6.1-sol","input":[
        {"role":"user","content":"retained ".repeat(2000)},
        {"type":"reasoning","summary":[],"encrypted_content":"synthetic".repeat(10000)}]});
    let mut quantities = crate::proxy_quantity::InputMeasurements::default();
    quantities.observe(&source, &json!({"input_tokens":10000}), "route", None, 100);
    let mut target = source.clone();
    target["input"].as_array_mut().unwrap().pop();
    let baseline = quantities.predict(&source, "route", None).unwrap();
    let candidate = quantities
        .projected_estimate(&target, &source, "route", None)
        .unwrap();
    let costs = resolved_input_costs(
        &source,
        &target,
        &[],
        "gpt-6.1-sol",
        3,
        None,
        "synthetic",
        100,
        baseline,
        candidate,
    )
    .unwrap();
    let rates = Rates::for_model("gpt-6.1-sol", candidate.tokens as f64).unwrap();
    assert!(
        (costs.compact - candidate.tokens as f64 * rates.input * 3.0).abs() < 1e-12,
        "forecast allocation is not proof that a future cache write covers this prompt: {costs:?}"
    );
}

#[test]
fn fresh_reasoning_baseline_is_forecast_only_and_continues_after_restart() {
    use crate::proxy_quantity::{EstimateBasis, InputMeasurements};
    let source = json!({"model":"gpt-6.1-sol","input":[
        {"role":"user","content":"goal"},
        {"type":"reasoning","summary":[],"encrypted_content":"old synthetic".repeat(1000)},
        {"role":"assistant","content":"old evidence"},
        {"role":"user","content":"current"}]});
    let mut quantities = InputMeasurements::default();
    quantities.observe(&source, &json!({"input_tokens":10000}), "route", None, 100);
    let mut appended = source.clone();
    appended["input"].as_array_mut().unwrap().extend([
        json!({"type":"reasoning","summary":[],"encrypted_content":"fresh synthetic".repeat(1000)}),
        json!({"role":"assistant","content":"fresh answer"}),
        json!({"role":"user","content":"continue"}),
    ]);
    for restart in [false, true] {
        if restart {
            quantities =
                serde_json::from_value(serde_json::to_value(&quantities).unwrap()).unwrap();
        }
        let frozen = serde_json::to_value(&quantities).unwrap();
        assert!(quantities.predict(&appended, "route", None).is_none());
        assert!(quantities.estimate(&appended, "route", None).is_none());
        assert!(
            quantities
                .known_estimate(&appended, "route", None)
                .is_none()
        );
        let baseline = quantities
            .forecast_estimate(&appended, "route", None)
            .unwrap();
        assert_eq!(
            baseline.basis,
            EstimateBasis::ReceiptAnchoredReasoningForecast
        );
        assert!(baseline.tokens > 10000);
        let mut target = appended.clone();
        target["input"].as_array_mut().unwrap().drain(1..3);
        let candidate = quantities
            .projected_estimate(&target, &appended, "route", None)
            .unwrap();
        assert_eq!(
            candidate.basis,
            EstimateBasis::ReceiptAnchoredReasoningForecast
        );
        let neutral = quantities
            .accounting_restoration(&target, &appended, "route", None)
            .unwrap();
        assert_eq!(
            candidate.tokens as i128,
            baseline.tokens as i128 + neutral.tokens
        );
        assert_eq!(candidate.capacity_tokens(), None);
        assert!(candidate.tokens < baseline.tokens);
        assert!(
            quantities
                .restoration(&appended, &target, "route", None)
                .is_none()
        );
        let costs = resolved_input_costs(
            &appended,
            &target,
            &[],
            "gpt-6.1-sol",
            3,
            None,
            "synthetic",
            100,
            baseline,
            candidate,
        )
        .unwrap();
        let rates = Rates::for_model("gpt-6.1-sol", baseline.tokens as f64).unwrap();
        assert!((costs.keep - baseline.tokens as f64 * rates.input * 3.0).abs() < 1e-12);
        assert!((costs.compact - candidate.tokens as f64 * rates.input * 3.0).abs() < 1e-12);
        assert_eq!(serde_json::to_value(&quantities).unwrap(), frozen);
        for key in ["model", "instructions", "tools", "reasoning"] {
            let mut mismatch = appended.clone();
            mismatch[key] = json!("changed");
            assert!(
                quantities
                    .forecast_estimate(&mismatch, "route", None)
                    .is_none()
            );
        }
        assert!(
            quantities
                .forecast_estimate(&appended, "other-route", None)
                .is_none()
        );
        for opaque in [
            json!({"type":"compaction","encrypted_content":"unknown"}),
            json!({"type":"input_image","image_url":"synthetic"}),
            json!({"type":"reasoning","encrypted_content":""}),
        ] {
            let mut unsupported = appended.clone();
            unsupported["input"].as_array_mut().unwrap().push(opaque);
            assert!(
                quantities
                    .forecast_estimate(&unsupported, "route", None)
                    .is_none()
            );
        }
        let mut restored = appended.clone();
        restored["input"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"role":"assistant","content":"not an append"}));
        assert!(
            quantities
                .forecast_estimate(&restored, "route", None)
                .is_none()
        );
        let mut duplicate = appended.clone();
        duplicate["input"]
            .as_array_mut()
            .unwrap()
            .push(source["input"][1].clone());
        assert!(
            quantities
                .forecast_estimate(&duplicate, "route", None)
                .is_none()
        );
    }
    let mut conflicting = quantities.clone();
    conflicting.observe(&source, &json!({"input_tokens":11000}), "route", None, 101);
    assert!(
        conflicting
            .forecast_estimate(&appended, "route", None)
            .is_none()
    );
    quantities.observe(
        &appended,
        &json!({"input_tokens":12500}),
        "route",
        None,
        101,
    );
    assert_eq!(
        quantities
            .forecast_estimate(&appended, "route", None)
            .unwrap()
            .basis,
        EstimateBasis::ProviderReceipt
    );
}

#[test]
fn fresh_reasoning_baseline_prices_authorized_projection_without_capacity_authority() {
    let (mut config, _, _) = fixture(Rates {
        input: 2.0,
        cached: 0.2,
        write: 2.0,
        output: 0.0,
    });
    config.payoff_requests = 1;
    config.min_payback_percent = 0;
    let mut session = Session {
        model_resolution: config.model_resolution.clone(),
        ..Session::default()
    };
    let mut body = json!({"model":"gpt-6-sol","input":[
        {"role":"user","content":"goal"},
        {"type":"reasoning","summary":[],"encrypted_content":"old synthetic".repeat(1000)},
        {"type":"function_call","call_id":"old","name":"inspect","arguments":"{}"},
        {"type":"function_call_output","call_id":"old","output":"evidence"},
        {"role":"user","content":"current"}]});
    session.ingest(body["input"].as_array().unwrap()).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":[2],"memories":[]}),
        )
        .unwrap();
    session.completed_requests = 1;
    session.primary_input.observe(
        &primary_wire_body(&config, &body),
        &json!({"input_tokens":10000}),
        &config.upstream_url,
        config.model_resolution.as_ref().map(|p| &p.primary),
        100,
    );
    // A known oversized baseline cannot force an uncertain projected fit.
    config.primary_context_tokens = 500;
    let (selected, report) = plan(&config, &session, &body);
    assert_eq!(report["forced"], true);
    assert!(selected.is_none());
    body["input"].as_array_mut().unwrap().extend([
        json!({"type":"reasoning","summary":[],"encrypted_content":"fresh synthetic".repeat(1000)}),
        json!({"role":"assistant","content":"fresh answer"}),
        json!({"role":"user","content":"continue"}),
    ]);
    session.ingest(body["input"].as_array().unwrap()).unwrap();
    for restart in [false, true] {
        if restart {
            session = serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
            // Session JSON deliberately omits index-owned advice. This unit
            // seam must reload the same current authority after its round-trip;
            // the real history-index restart is exercised by the HTTP probe.
            session
                .apply_advice(
                    &session.groups(),
                    &json!({"protected":[],"removable":[2],"memories":[]}),
                )
                .unwrap();
        }
        let frozen = serde_json::to_value(&session).unwrap();
        let (selected, report) = plan(&config, &session, &body);
        assert_eq!(
            selected
                .unwrap_or_else(|| {
                    panic!(
                        "authorized fresh-reasoning projection missing; restart={restart}; report={report}; drop_members={:?}; opinions={:?}; groups={:?}",
                        session.drop_members,
                        session.opinions,
                        session.groups()
                    )
                })
                .omitted,
            HashSet::from([2, 3, 4]),
            "{report}"
        );
        assert_eq!(
            report["primary_input_basis"],
            "receipt_anchored_reasoning_forecast"
        );
        assert!(report["primary_estimated_tokens"].as_u64().unwrap() > 10000);
        assert_eq!(report["forced"], false);
        assert!(report["primary_required_tokens"].is_null());
        assert!(report["primary_fits"].is_null());
        assert!(report["primary_receipt_prediction_tokens"].is_null());
        assert!(report["candidates"][0]["primary_retained_estimated_tokens"].is_number());
        assert!(report["candidates"][0]["primary_retained_required_tokens"].is_null());
        assert_eq!(serde_json::to_value(&session).unwrap(), frozen);
    }
    session.opinions.insert(2, "keep".into());
    assert!(
        plan(&config, &session, &body).0.is_none(),
        "forecast grants no removal authority"
    );
}
