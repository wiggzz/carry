// Receipt aliases describe one native partition, regardless of monetary price.
// These exercise the real observation/forecast owner, not a parallel decoder.
fn partition_credit(usage: Value, valid: bool) {
    use carry::models::{Route, resolve_model};
    let mut model = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    // Unknown monetary pricing must not erase valid usage/cache proof. Output
    // usage is deliberately absent: cache partitions are independent quantities.
    model.rates = None;
    model.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 7;
    let body = json!({"model":"gpt-6.1-sol", "input":[
        {"role":"user","content":"synthetic alias prefix ".repeat(2000)}]});
    let original = usage.clone();
    let mut cache = Vec::new();
    observe(
        &mut cache,
        &body,
        &usage,
        Some(&model),
        "scope",
        ObservationTime::during(100, 103),
    );
    assert_eq!(
        usage, original,
        "raw native observation must not be rewritten"
    );
    let encoded = serde_json::to_vec(&cache).unwrap();
    cache = serde_json::from_slice(&encoded).unwrap();
    let credit = crate::proxy_savings::token_units(&body["input"]) as f64;
    assert!(credit > 0.0);
    assert_eq!(
        reusable(&cache, &body, Some(&model), "scope", 104),
        if valid { credit } else { 0.0 },
        "CACHE_PARTITION_PROOF: only an agreed, bounded whole write can supply credit"
    );
    if valid {
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].native_input_tokens, Some(2000));
        assert_eq!(cache[0].native_write_tokens, Some(2000));
        assert_eq!(cache[0].native_cached_fraction, 0.0);
        assert_eq!(cache[0].write_window.unwrap().not_before, 100);
    } else {
        assert!(cache.iter().all(|e| e.write_window.is_none()));
    }
    assert_eq!(reusable(&cache, &body, Some(&model), "scope", 107), 0.0);
    assert_eq!(serde_json::to_vec(&cache).unwrap(), encoded);
}

#[test]
fn conflicting_write_aliases_cannot_mint_timed_cache_proof() {
    partition_credit(
        json!({"input_tokens":2000,"input_tokens_details":{
            "cached_tokens":0,"cache_write_tokens":2000,"cache_creation_tokens":1}}),
        false,
    );
}

#[test]
fn conflicting_read_aliases_cannot_mint_timed_cache_proof() {
    partition_credit(
        json!({"input_tokens":2000,"cached_input_tokens":1,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":2000}}),
        false,
    );
}

#[test]
fn malformed_write_alias_cannot_mint_timed_cache_proof() {
    partition_credit(
        json!({"input_tokens":2000,"cache_creation_input_tokens":"2000",
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":2000}}),
        false,
    );
}

#[test]
fn overlapping_read_write_partition_cannot_mint_timed_cache_proof() {
    partition_credit(
        json!({"input_tokens":2000,
            "input_tokens_details":{"cached_tokens":1,"cache_write_tokens":2000}}),
        false,
    );
}

#[test]
fn agreed_aliases_preserve_cache_proof_without_money_or_output_usage() {
    partition_credit(
        json!({"input_tokens":2000,"cached_input_tokens":0,
            "cache_write_input_tokens":2000,"cache_creation_input_tokens":2000,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":2000,
                "cache_creation_tokens":2000}}),
        true,
    );
}

#[test]
fn alternate_write_alias_preserves_cache_proof_without_money_or_output_usage() {
    partition_credit(
        json!({"input_tokens":2000,
            "input_tokens_details":{"cached_tokens":0,"cache_creation_tokens":2000}}),
        true,
    );
}

#[test]
fn alternate_read_and_write_aliases_preserve_cache_proof_without_price() {
    partition_credit(
        json!({"input_tokens":2000,"cached_input_tokens":0,"cache_creation_input_tokens":2000}),
        true,
    );
}

fn partition_settlement(usage: Value, valid: bool) {
    let mut ledger = carry::core::UsageLedger::default();
    let rates = carry::models::Rates {
        input: 2.0 / 1e6,
        cached: 0.1 / 1e6,
        write: 2.5 / 1e6,
        output: 10.0 / 1e6,
    };
    let original = usage.clone();
    let settled = ledger.settle_with_rates("synthetic", &usage, true, Some(rates));
    assert_eq!(usage, original);
    assert_eq!(ledger.calls, 1);
    assert_eq!(ledger.input_tokens, 2000);
    assert_eq!(ledger.output_tokens, 10);
    assert_eq!(ledger.unavailable_cost_calls, u64::from(!valid));
    if valid {
        let expected = 2000.0 * rates.write + 10.0 * rates.output;
        assert!((settled.unwrap() - expected).abs() < 1e-12);
        assert!((ledger.cost_usd - expected).abs() < 1e-12);
        assert_eq!(ledger.cache_write_tokens, 2000);
    } else {
        assert!(
            settled.is_none(),
            "CACHE_PARTITION_SETTLEMENT: invalid partitions are not priced"
        );
        assert_eq!(ledger.cost_usd, 0.0);
    }
    // A validated read quantity remains independently observable when only the
    // write aliases conflict. No cache fraction may be selected from bad reads.
    let valid_read = usage["cached_input_tokens"].as_u64() != Some(1);
    assert_eq!(ledger.cache_observation.calls, u64::from(valid_read));
    assert_eq!(
        ledger.cache_observation.input_tokens,
        if valid_read { 2000 } else { 0 }
    );
    let restored: carry::core::UsageLedger =
        serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
    assert_eq!(restored.cost_usd, ledger.cost_usd);
    assert_eq!(
        restored.unavailable_cost_calls,
        ledger.unavailable_cost_calls
    );
}

#[test]
fn monetary_settlement_rejects_conflicting_write_aliases() {
    partition_settlement(
        json!({"input_tokens":2000,"output_tokens":10,"input_tokens_details":{
            "cached_tokens":0,"cache_write_tokens":2000,"cache_creation_tokens":1}}),
        false,
    );
}

#[test]
fn monetary_settlement_rejects_conflicting_read_aliases() {
    partition_settlement(
        json!({"input_tokens":2000,"output_tokens":10,"cached_input_tokens":1,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":2000}}),
        false,
    );
}

#[test]
fn monetary_settlement_rejects_malformed_optional_write_aliases() {
    partition_settlement(
        json!({"input_tokens":2000,"output_tokens":10,"cache_creation_input_tokens":"2000",
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":2000}}),
        false,
    );
}

#[test]
fn monetary_settlement_accepts_alternate_cache_aliases() {
    partition_settlement(
        json!({"input_tokens":2000,"output_tokens":10,"cached_input_tokens":0,
            "cache_creation_input_tokens":2000}),
        true,
    );
}
