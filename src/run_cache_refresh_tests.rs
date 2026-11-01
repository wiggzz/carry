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
    cache.begin_request_with_refresh(
        generation(),
        &history,
        2400,
        backend.cache_read_refresh_seconds(&body),
    );
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
    cache.begin_request_with_refresh(
        generation(),
        &history,
        2400,
        backend.cache_read_refresh_seconds(&body),
    );
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
    let read = cache.read.as_ref().unwrap();
    assert_eq!(read.native_tokens, 5000);
    assert_eq!(read.request_tokens, 2400);
    assert_eq!(read.history, history);
    assert!(read.window.eligible_at(Instant::now()));
    assert!(!read.window.eligible_at(read.window.origin + CACHE_TTL));
    assert!(
        !read
            .window
            .eligible_at(read.window.origin - Duration::from_nanos(1))
    );
    assert_eq!(cache.implicit_write, Some(original));
    assert_eq!(cache.breakpoints[&7].write, Some(original));
}

#[test]
fn native_read_contract_uses_actual_client_route_auth_model_and_request() {
    let history = [json!({"role":"user","content":"synthetic route input"})];
    let backend = native_refresh_backend();
    let body = backend.request_body("fixed", &history).unwrap();
    assert_eq!(backend.cache_read_refresh_seconds(&body), Some(1800));
    for (base, auth, model) in [
        (
            "http://127.0.0.1:9/v1",
            crate::openai::RequestAuth::ApiKey("not-sent".into()),
            "gpt-6-sol",
        ),
        (
            "https://api.openai.com/v1",
            crate::openai::RequestAuth::ApiKey("not-sent".into()),
            "unknown-native-model",
        ),
        (
            "https://chatgpt.com/backend-api/codex",
            crate::openai::RequestAuth::CodexSubscription {
                access_token: "not-sent".into(),
                account_id: "synthetic".into(),
                credential_home: None,
            },
            "gpt-6-sol",
        ),
    ] {
        let backend = Backend::openai(
            OpenAiClient::with_timeouts_and_prompt_cache_key(
                base.into(),
                auth,
                model.into(),
                "medium".into(),
                "synthetic-native-key".into(),
                Duration::from_secs(5),
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let body = backend.request_body("fixed", &history).unwrap();
        assert_eq!(
            backend.cache_read_refresh_seconds(&body),
            None,
            "{base}/{model}"
        );
    }
    for (key, value) in [
        ("model", json!("gpt-6-luna")),
        ("prompt_cache_options", json!({"mode":"unknown"})),
        (
            "prompt_cache_options",
            json!({"mode":"implicit","ttl":"12h"}),
        ),
    ] {
        let mut changed = body.clone();
        changed[key] = value;
        assert_eq!(backend.cache_read_refresh_seconds(&changed), None);
    }
}

#[test]
fn native_read_coverage_requires_whole_native_extent_contract_and_bounded_time() {
    let backend = native_refresh_backend();
    let history = [json!({"role":"user","content":"synthetic exact extent"})];
    let body = backend.request_body("fixed", &history).unwrap();
    let refresh = backend.cache_read_refresh_seconds(&body);
    for (receipt, contract, timing, expected) in [
        (
            json!({"input_tokens":5000,"cached_input_tokens":5000}),
            refresh,
            0,
            2400,
        ),
        (
            json!({"input_tokens":5000,"cached_input_tokens":5000,"cache_write_input_tokens":"invalid"}),
            refresh,
            0,
            2400,
        ),
        (
            json!({"input_tokens":5000,"cached_input_tokens":4999}),
            refresh,
            0,
            0,
        ),
        (
            json!({"input_tokens":5000,"cached_input_tokens":5000,"input_tokens_details":{"cached_tokens":1}}),
            refresh,
            0,
            0,
        ),
        (
            json!({"input_tokens":100,"cached_input_tokens":100}),
            refresh,
            0,
            0,
        ),
        (
            json!({"input_tokens":5000,"cached_input_tokens":5000}),
            None,
            0,
            0,
        ),
        (
            json!({"input_tokens":5000,"cached_input_tokens":5000}),
            refresh,
            1,
            0,
        ),
        (
            json!({"input_tokens":5000,"cached_input_tokens":5000}),
            refresh,
            2,
            0,
        ),
    ] {
        let mut cache = CacheTracker::new(backend.prompt_cache_capabilities());
        cache.begin_request_with_refresh(
            vec![RenderedBreakpoint {
                generation: 7,
                prefix_tokens: 1200,
            }],
            &history,
            2400,
            contract,
        );
        if timing == 1 {
            cache.pending_write = None;
        }
        if timing == 2 {
            cache.pending_write = Some((Instant::now() - CACHE_TTL, CACHE_TTL));
        }
        let original = receipt.clone();
        cache.observe_receipt(&receipt);
        assert_eq!(receipt, original);
        assert!(cache.implicit_write.is_none());
        assert_eq!(
            cache
                .policy_for_history(&history, 1, 0)
                .implicit_cached_tokens,
            expected,
            "receipt={receipt}, contract={contract:?}, timing={timing}"
        );
        assert_eq!(cache.policy().breakpoints.len(), usize::from(expected > 0));
    }
}

#[test]
fn native_read_lifecycle_reconciles_prefix_miss_compaction_and_cold_resume() {
    let backend = native_refresh_backend();
    let mut context = ContextState::new("native protected direction ".repeat(800));
    let removable = scope_tool(&mut context, "refresh-removable");
    let kept = scope_tool(&mut context, "refresh-kept");
    context.record_signals(&scope_signals(&[kept], &[removable]), kept);
    let history = context.input_items();
    let body = backend.request_body("fixed", &history).unwrap();
    let mut cache = CacheTracker::new(backend.prompt_cache_capabilities());
    cache.begin_request_with_refresh(
        context.rendered_breakpoints(),
        &history,
        context.estimated_tokens(),
        backend.cache_read_refresh_seconds(&body),
    );
    cache.observe_receipt(
        &json!({"input_tokens":50000,"cached_input_tokens":50000,"cache_write_input_tokens":0}),
    );
    let warm = cache.policy_for_history(&history, 1, 0);
    assert_eq!(warm.implicit_cached_tokens, context.estimated_tokens());
    assert!(!warm.breakpoints.is_empty());
    context.add_user("ordinary appended task".into());
    let appended = context.input_items();
    assert_eq!(
        cache
            .policy_for_history(&appended, 1, 0)
            .implicit_cached_tokens,
        warm.implicit_cached_tokens
    );
    let changed = [json!({"role":"user","content":"different native prefix"})];
    assert_eq!(
        cache
            .policy_for_history(&changed, 1, 0)
            .implicit_cached_tokens,
        0
    );
    // Unknown observations preserve existing fact; a validated newer miss does not.
    cache.begin_request_with_history(
        context.rendered_breakpoints(),
        &appended,
        context.estimated_tokens(),
    );
    cache.observe_receipt(&json!({}));
    assert_eq!(
        cache
            .policy_for_history(&appended, 1, 0)
            .implicit_cached_tokens,
        warm.implicit_cached_tokens
    );
    cache.begin_request_with_history(
        context.rendered_breakpoints(),
        &appended,
        context.estimated_tokens(),
    );
    cache.observe_receipt(
        &json!({"input_tokens":50000,"cached_input_tokens":0,"cache_write_input_tokens":0}),
    );
    assert_eq!(
        cache
            .policy_for_history(&appended, 1, 0)
            .implicit_cached_tokens,
        0
    );
    assert!(cache.policy().breakpoints.is_empty());
    // After the real newer miss, the ContextState owner selects a cold rewrite.
    let plan = context
        .plan_compaction(&[], cache.policy_for_history(&appended, 1, 0))
        .unwrap();
    let change = context.compact(plan);
    cache.mark_compaction(&change.invalidated_generations);
    let current = context.input_items();
    assert!(!current.starts_with(&history));
    assert_eq!(
        cache
            .policy_for_history(&current, 1, 0)
            .implicit_cached_tokens,
        0
    );
    assert!(
        cache
            .policy()
            .breakpoints
            .iter()
            .all(|b| !change.invalidated_generations.contains(&b.generation))
    );
    assert!(
        !current
            .iter()
            .any(|item| item["call_id"] == "refresh-removable")
    );
    assert_eq!(
        current
            .iter()
            .filter(|item| item["call_id"] == "refresh-kept")
            .count(),
        2
    );
    let resumed = ContextState::decode(&context.encode().unwrap()).unwrap();
    assert_eq!(resumed.input_items(), current);
    let cold = CacheTracker::new(backend.prompt_cache_capabilities());
    assert_eq!(
        cold.policy_for_history(&current, 1, 0)
            .implicit_cached_tokens,
        0
    );
    assert!(cold.policy().breakpoints.is_empty());
}
