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
