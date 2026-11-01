// Exact official-route client construction, but no network/provider calls.
// Native usage, monotonic age and local extents are explicitly synthetic.
fn native_refresh_backend() -> Backend {
    Backend::openai(
        OpenAiClient::with_timeouts_and_prompt_cache_key(
            "https://api.openai.com/v1".into(),
            crate::openai::RequestAuth::ApiKey("synthetic-never-sent".into()),
            "gpt-6-sol".into(),
            "medium".into(),
            "synthetic-native-refresh".into(),
            Duration::from_secs(5),
            Duration::from_secs(1),
        )
        .unwrap(),
    )
}

#[test]
fn native_qualified_full_read_has_separate_coverage_after_original_write_expiry() {
    let backend = native_refresh_backend();
    let capabilities = backend.prompt_cache_capabilities().unwrap();
    let history = [json!({"role":"user","content":"exact native input ".repeat(600)})];
    let body = backend
        .request_body("fixed synthetic system", &history)
        .unwrap();
    let resolved = carry::models::resolve_model(
        "gpt-6-sol",
        carry::models::Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    assert_eq!(resolved.cache_read_refresh_seconds(&body), Some(1800));
    let generation = || {
        vec![RenderedBreakpoint {
            generation: 7,
            prefix_tokens: 1200,
        }]
    };
    let mut cache = CacheTracker::new(Some(capabilities));
    cache.begin_request_with_history(generation(), &history, 2400);
    let write_receipt = json!({"input_tokens":5000,"output_tokens":1,
        "cached_input_tokens":0,"cache_write_input_tokens":5000});
    cache.observe_receipt(&write_receipt);
    assert_eq!(
        cache
            .policy_for_history(&history, 1, 0)
            .implicit_cached_tokens,
        2400
    );
    assert_eq!(
        cache.policy().breakpoints,
        vec![PricedBreakpoint {
            generation: 7,
            cached_tokens: 1200
        }]
    );

    // Synthetic clock aging of the actual original fact; the following full
    // read is independent coverage, never a replacement native write.
    let expired = Instant::now() - CACHE_TTL;
    let original = MonotonicWrite::during(expired, expired, CACHE_TTL).unwrap();
    cache.implicit_write = Some(original);
    cache.breakpoints.get_mut(&7).unwrap().write = Some(original);
    assert_eq!(
        cache
            .policy_for_history(&history, 1, 0)
            .implicit_cached_tokens,
        0
    );
    cache.begin_request_with_history(generation(), &history, 2400);
    let read_receipt = json!({"input_tokens":5000,"output_tokens":1,
        "cached_input_tokens":5000,"cache_write_input_tokens":0});
    let read_bytes = serde_json::to_vec(&read_receipt).unwrap();
    cache.observe_receipt(&read_receipt);
    assert_eq!(cache.implicit_write, Some(original));
    assert_eq!(cache.breakpoints[&7].write, Some(original));
    assert_eq!(write_receipt["cache_write_input_tokens"], 5000);
    assert_eq!(serde_json::to_vec(&read_receipt).unwrap(), read_bytes);
    let mut ledger = carry::core::UsageLedger::default();
    let rates = resolved.rates_for_input(5000.0).unwrap();
    let cost = ledger
        .settle_with_rates("gpt-6-sol", &read_receipt, true, Some(rates))
        .unwrap();
    assert_eq!(ledger.cache_write_tokens, 0);
    assert!((cost - (5000.0 * rates.cached + rates.output)).abs() < 1e-12);
    assert_eq!(
        cache
            .policy_for_history(&history, 1, 0)
            .implicit_cached_tokens,
        2400,
        "NATIVE_QUALIFIED_READ_COVERAGE: documented full read must have independent bounded coverage, not rewrite the original write",
    );
    assert_eq!(
        cache.policy().breakpoints,
        vec![PricedBreakpoint {
            generation: 7,
            cached_tokens: 1200
        }]
    );
}
