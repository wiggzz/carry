// Provider-contract seam: official exact-route metadata, synthetic receipt/time.
// This is not a provider probe or proof of physical KV residency/latency.
#[test]
fn qualified_full_read_refresh_has_its_own_interval_without_rewriting_original_write() {
    use carry::models::{Route, resolve_model};
    let model = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let body = json!({"model":"gpt-6.1-sol","prompt_cache_key":"synthetic-scoped-refresh",
        "input":[{"role":"user","content":"exact whole written prefix ".repeat(2000)}]});
    let written = json!({"input_tokens":5000,"output_tokens":1,"cached_input_tokens":0,"cache_write_input_tokens":5000});
    let read = json!({"input_tokens":5000,"output_tokens":1,"cached_input_tokens":5000,"cache_write_input_tokens":0});
    let mut cache = Vec::new();
    assert!(observe(
        &mut cache,
        &body,
        &written,
        Some(&model),
        "auth",
        ObservationTime::during(100, 103)
    ));
    let original = serde_json::to_value(&cache[0]).unwrap();
    let credit = reusable(&cache, &body, Some(&model), "auth", 1898);
    assert!(credit > 0.0);
    assert_eq!(reusable(&cache, &body, Some(&model), "auth", 1900), 0.0);
    assert!(observe(
        &mut cache,
        &body,
        &read,
        Some(&model),
        "auth",
        ObservationTime::during(1898, 1899)
    ));
    assert!(
        cache
            .iter()
            .any(|fact| serde_json::to_value(fact).unwrap() == original),
        "original write fact is immutable"
    );
    assert_eq!(written["cache_write_input_tokens"], 5000);
    assert_eq!(read["cache_write_input_tokens"], 0);
    let encoded = serde_json::to_vec(&cache).unwrap();
    let resumed: Vec<CacheEvidence> = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        reusable(&resumed, &body, Some(&model), "other-auth", 1901),
        0.0
    );
    let mut other = body.clone();
    other["input"][0]["content"] = json!("genuinely changed prefix");
    assert_eq!(reusable(&resumed, &other, Some(&model), "auth", 1901), 0.0);
    // Conservative interval: unavailable before completion and at earliest expiry.
    assert_eq!(reusable(&resumed, &body, Some(&model), "auth", 3698), 0.0);
    assert_eq!(serde_json::to_vec(&resumed).unwrap(), encoded);
    assert_eq!(
        reusable(&resumed, &body, Some(&model), "auth", 1901),
        credit,
        "QUALIFIED_READ_REFRESH: bounded full read has independent documented billing coverage, not a rewritten write"
    );
}

fn refresh_fixture() -> (carry::models::ResolvedModel, Value, Value) {
    let model = carry::models::resolve_model(
        "gpt-6.1-sol",
        carry::models::Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let body = json!({"model":"gpt-6.1-sol","prompt_cache_key":"separate-read",
        "input":[{"role":"user","content":"whole synthetic prefix ".repeat(2000)}]});
    let read = json!({"input_tokens":5000,"output_tokens":1,
        "cached_input_tokens":5000,"cache_write_input_tokens":0});
    (model, body, read)
}

#[test]
fn bounded_full_read_needs_no_prior_write_or_scenario_price_and_never_adds_write_money() {
    let (mut model, body, read) = refresh_fixture();
    let rates = model.rates_for_input(5000.0).unwrap();
    model.rates = None;
    model.scenario_cache_policy = None;
    for writes in [None, Some(json!(0)), Some(json!("invalid"))] {
        let mut usage = read.clone();
        usage
            .as_object_mut()
            .unwrap()
            .remove("cache_write_input_tokens");
        if let Some(write) = writes {
            usage["cache_write_input_tokens"] = write;
        }
        let original = usage.clone();
        let mut cache = Vec::new();
        assert!(observe(
            &mut cache,
            &body,
            &usage,
            Some(&model),
            "auth",
            ObservationTime::during(100, 103)
        ));
        assert_eq!(usage, original);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].native_read_tokens, Some(5000));
        assert!(cache[0].write_window.is_none());
        let saved = serde_json::to_vec(&cache).unwrap();
        let cache: Vec<CacheEvidence> = serde_json::from_slice(&saved).unwrap();
        let credit = crate::proxy_savings::token_units(&body["input"]) as f64;
        for at in [99, 100, 102, 103, 1899, 1900, 5000] {
            assert_eq!(
                reusable(&cache, &body, Some(&model), "auth", at),
                if (103..1900).contains(&at) {
                    credit
                } else {
                    0.0
                }
            );
            assert_eq!(serde_json::to_vec(&cache).unwrap(), saved);
        }
        let mut ledger = carry::core::UsageLedger::default();
        let cost = ledger.settle_with_rates("gpt-6.1-sol", &usage, true, Some(rates));
        assert_eq!(ledger.calls, 1);
        assert_eq!(ledger.cache_write_tokens, 0);
        if usage["cache_write_input_tokens"] == 0 {
            assert!((cost.unwrap() - (5000.0 * rates.cached + rates.output)).abs() < 1e-12);
        } else {
            assert!(
                cost.is_none(),
                "missing/invalid write money remains unknown independently of read proof"
            );
        }
    }
}

#[test]
fn unqualified_reads_cannot_borrow_scenario_or_injected_cache_entitlement() {
    let (official, body, read) = refresh_fixture();
    let mut models = Vec::new();
    for (endpoint, subscribed) in [
        ("https://api.openai.com/v1/responses/", false),
        ("http://127.0.0.1:9/v1/responses", false),
        ("https://chatgpt.com/backend-api/codex/responses", true),
    ] {
        let mut model = carry::models::resolve_model(
            "gpt-6.1-sol",
            carry::models::Route::new(endpoint, subscribed),
            None,
            None,
            0,
            0,
        )
        .unwrap();
        model.cache = official.cache; // synthetic capability is not a provider contract
        models.push(model);
    }
    let mut unknown = carry::models::resolve_model(
        "unknown-refresh-model",
        official.route.clone(),
        None,
        Some(10000),
        0,
        0,
    )
    .unwrap();
    unknown.cache = official.cache;
    models.push(unknown);
    for model in models {
        assert!(model.cache_read_refresh_seconds(&body).is_none());
        let mut cache = Vec::new();
        assert!(observe(
            &mut cache,
            &body,
            &read,
            Some(&model),
            "auth",
            ObservationTime::during(100, 103)
        ));
        assert!(cache[0].read_window.is_none());
        assert_eq!(reusable(&cache, &body, Some(&model), "auth", 104), 0.0);
    }
    for controls in [
        json!({"mode":"unknown"}),
        json!({"mode":null}),
        json!({"ttl":"12h"}),
        json!(null),
    ] {
        let mut other = body.clone();
        other["prompt_cache_options"] = controls;
        assert!(official.cache_read_refresh_seconds(&other).is_none());
    }
    for controls in [
        json!({}),
        json!({"mode":"implicit"}),
        json!({"mode":"explicit","ttl":"30m"}),
    ] {
        let mut other = body.clone();
        other["prompt_cache_options"] = controls;
        assert_eq!(official.cache_read_refresh_seconds(&other), Some(1800));
    }
    let mut other = body.clone();
    other["model"] = json!("gpt-6-luna");
    assert!(official.cache_read_refresh_seconds(&other).is_none());
}

#[test]
fn read_coverage_rejects_unknown_time_partial_conflicting_and_overlong_intervals() {
    let (model, body, read) = refresh_fixture();
    for time in [
        100u64.into(),
        ObservationTime::during(104, 103),
        ObservationTime::during(100, 1900),
    ] {
        let mut cache = Vec::new();
        assert!(observe(
            &mut cache,
            &body,
            &read,
            Some(&model),
            "auth",
            time
        ));
        for at in [100, 103, 104, 1900, 2000] {
            assert_eq!(reusable(&cache, &body, Some(&model), "auth", at), 0.0);
        }
    }
    for usage in [
        json!({"input_tokens":5000,"cached_input_tokens":4999,"cache_write_input_tokens":0}),
        json!({"input_tokens":5000,"cached_input_tokens":5000,"input_tokens_details":{"cached_tokens":1}}),
        json!({"input_tokens":u64::MAX,"cached_input_tokens":u64::MAX-1,"cache_write_input_tokens":0}),
    ] {
        let mut cache = Vec::new();
        observe(
            &mut cache,
            &body,
            &usage,
            Some(&model),
            "auth",
            ObservationTime::during(100, 103),
        );
        assert_eq!(
            reusable(&cache, &body, Some(&model), "auth", 104),
            0.0,
            "native integer extent, not rounded cached fraction, owns full-read proof"
        );
    }
}

#[test]
fn shorter_read_preserves_later_existing_coverage_without_restarting_its_long_clock() {
    let (model, body, read) = refresh_fixture();
    let mut cache = Vec::new();
    let write =
        json!({"input_tokens":5000,"cached_input_tokens":0,"cache_write_input_tokens":5000});
    assert!(observe(
        &mut cache,
        &body,
        &write,
        Some(&model),
        "auth",
        ObservationTime::during(100, 103)
    ));
    // Synthetic independently known longer prior interval, NOT a supported
    // 12-hour producer/tariff claim. Current new long writes remain unsupported.
    cache[0].write_window.as_mut().unwrap().lifetime = 43200;
    let original = serde_json::to_value(&cache[0]).unwrap();
    assert!(observe(
        &mut cache,
        &body,
        &read,
        Some(&model),
        "auth",
        ObservationTime::during(2000, 2001)
    ));
    assert!(
        cache
            .iter()
            .any(|fact| serde_json::to_value(fact).unwrap() == original)
    );
    let encoded = serde_json::to_vec(&cache).unwrap();
    let resumed: Vec<CacheEvidence> = serde_json::from_slice(&encoded).unwrap();
    let credit = crate::proxy_savings::token_units(&body["input"]) as f64;
    assert_eq!(
        reusable(&resumed, &body, Some(&model), "auth", 3800),
        credit
    );
    assert_eq!(
        reusable(&resumed, &body, Some(&model), "auth", 43299),
        credit
    );
    assert_eq!(reusable(&resumed, &body, Some(&model), "auth", 43300), 0.0);
    assert_eq!(serde_json::to_vec(&resumed).unwrap(), encoded);
}

#[test]
fn newer_known_miss_revokes_read_coverage_but_unknown_observation_does_not_mint_or_erase_it() {
    let (model, body, read) = refresh_fixture();
    let mut cache = Vec::new();
    assert!(observe(
        &mut cache,
        &body,
        &read,
        Some(&model),
        "auth",
        ObservationTime::during(100, 103)
    ));
    let saved = serde_json::to_vec(&cache).unwrap();
    assert!(!observe(
        &mut cache,
        &body,
        &json!({}),
        Some(&model),
        "auth",
        ObservationTime::during(104, 105)
    ));
    assert_eq!(serde_json::to_vec(&cache).unwrap(), saved);
    assert!(reusable(&cache, &body, Some(&model), "auth", 106) > 0.0);
    assert!(observe(
        &mut cache,
        &body,
        &json!({"input_tokens":5000,"cached_input_tokens":0,"cache_write_input_tokens":0}),
        Some(&model),
        "auth",
        ObservationTime::during(106, 107)
    ));
    assert_eq!(reusable(&cache, &body, Some(&model), "auth", 108), 0.0);
}
