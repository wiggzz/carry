// Issue188: explicit-only without breakpoints has no cache writes or reads.
// Default/implicit modes must retain supported paid writes. See the official
// prompt-caching guide; these are synthetic controls, not live billing evidence.
#[test]
fn cache_write188_native_report_no_write_control_prices_both_branches_ordinary() {
    let body = json!({"model":"gpt-6.1-sol",
        "prompt_cache_options":{"mode":"explicit", "ttl":"30m"},
        "input":[{"role":"user",
        "content":"fixed below-budget input ".repeat(1000)}]});
    let input = prompt_projection(&body).unwrap().2;
    assert!((1024..100_000).contains(&input));
    let mut t = Trajectory::default();
    t.record(&json!({"event":"run_started","data":{
        "model":"gpt-6.1-sol", "cache_ttl_seconds":1800,
        "implicit_cache_minimum_prefix_tokens":1024}}));
    t.record(&json!({"event":"model_request","run_id":"write-control",
        "timestamp_ms":100,"data":{"step":1,
        "transport":{"api_base":"https://api.openai.com/v1"},"request":body}}));
    let estimate = t
        .record(&json!({"event":"model_response","run_id":"write-control",
        "timestamp_ms":101,"data":{"step":1,"response_retries":0,
        "usage":{"input_tokens":input,"cached_input_tokens":0,
        "cache_write_input_tokens":0,"output_tokens":100},"raw":{"output":[]}}}))
        .unwrap();
    assert!(estimate.priced && estimate.observed_priced);
    assert_eq!(estimate.coverage.responses, 1);
    assert_eq!(estimate.pi_compactions, 0);
    assert_eq!(estimate.summary_cost, 0.0);
    assert_eq!(
        estimate.savings, 0.0,
        "equal simulated branches still cancel"
    );
    let ordinary = input as f64 * 2.0 / 1e6 + 100.0 * 10.0 / 1e6;
    assert!((estimate.observed_cost - ordinary).abs() < 1e-12);
    assert!(
        (estimate.carry_cost - ordinary).abs() < 1e-12,
        "CACHE_WRITE_188_NATIVE: no-write control must not add a premium to either simulated branch"
    );
    assert!((estimate.pi_cost - ordinary).abs() < 1e-12);
}

// Acceptance coverage for an already-supported charge, not a defect reproduction.
#[test]
fn cache_write188_native_report_preserves_paid_implicit_writes() {
    let body = json!({"model":"gpt-6.1-sol",
        "prompt_cache_key":"cache-write-188",
        "prompt_cache_options":{"mode":"implicit", "ttl":"30m"},
        "input":[{"role":"user", "content":"paid cold prefix ".repeat(1000)}]});
    let input = prompt_projection(&body).unwrap().2;
    assert!((1024..100_000).contains(&input));
    let mut t = Trajectory::default();
    t.record(&json!({"event":"run_started","data":{
        "model":"gpt-6.1-sol", "cache_ttl_seconds":1800,
        "implicit_cache_minimum_prefix_tokens":1024}}));
    t.record(&json!({"event":"model_request","run_id":"paid-control",
        "timestamp_ms":100,"data":{"step":1,
        "transport":{"api_base":"https://api.openai.com/v1"},"request":body}}));
    let estimate = t
        .record(&json!({"event":"model_response","run_id":"paid-control",
        "timestamp_ms":101,"data":{"step":1,"response_retries":0,
        "usage":{"input_tokens":input,"cached_input_tokens":0,
        "cache_write_input_tokens":input,"output_tokens":100},"raw":{"output":[]}}}))
        .unwrap();
    let paid = (input as f64 * 2.5 + 100.0 * 10.0) / 1e6;
    assert!(estimate.priced && estimate.observed_priced);
    assert_eq!(estimate.pi_compactions, 0);
    assert_eq!(estimate.summary_cost, 0.0);
    assert!((estimate.observed_cost - paid).abs() < 1e-12);
    assert!((estimate.carry_cost - paid).abs() < 1e-12);
    assert!((estimate.pi_cost - paid).abs() < 1e-12);
    assert_eq!(estimate.savings, 0.0);
}

// Acceptance controls for mode changes and unsupported request metadata.
#[test]
fn cache_write188_native_report_mode_switch_never_reuses_disabled_cache() {
    let mut body = json!({"model":"gpt-6.1-sol", "prompt_cache_key":"mode-switch",
        "input":[{"role":"user", "content":"cache boundary ".repeat(1000)}]});
    let input = prompt_projection(&body).unwrap().2;
    let mut t = Trajectory::with_model("gpt-6.1-sol".into());
    let mut expected = 0.0;
    for (index, mode) in ["implicit", "explicit", "explicit"].into_iter().enumerate() {
        body["prompt_cache_options"] = json!({"mode":mode, "ttl":"30m"});
        let writes = if index == 0 { input } else { 0 };
        t.record(&json!({"event":"model_request", "timestamp_ms":100+index,
            "data":{"step":index,"transport":{"api_base":"https://api.openai.com/v1"},"request":body}}));
        let e = t
            .record(&json!({"event":"model_response", "timestamp_ms":101+index,
            "data":{"step":index,"usage":{"input_tokens":input,"output_tokens":0,
            "cached_input_tokens":0,"cache_write_input_tokens":writes},"raw":{"output":[]}}}))
            .unwrap();
        expected += input as f64 * if index == 0 { 2.5 } else { 2.0 } / 1e6;
        assert!(e.priced && e.observed_priced);
        assert!((e.observed_cost - expected).abs() < 1e-12);
        assert!((e.carry_cost - expected).abs() < 1e-12);
        assert!((e.pi_cost - expected).abs() < 1e-12);
        assert_eq!(e.savings, 0.0);
        assert_eq!(e.counterfactual_cache_policy, "conditional_model_cache_v2");
    }
}

#[test]
fn cache_write188_native_report_unknown_mode_preserves_observed_ledger() {
    let body = json!({"model":"gpt-6.1-sol",
        "prompt_cache_options":{"mode":"unknown"},
        "input":[{"role":"user", "content":"unknown contract ".repeat(1000)}]});
    let input = prompt_projection(&body).unwrap().2;
    let mut t = Trajectory::with_model("gpt-6.1-sol".into());
    t.record(&json!({"event":"model_request","data":{"step":1,"request":body}}));
    let e = t
        .record(&json!({"event":"model_response","data":{"step":1,
        "usage":{"input_tokens":input,"output_tokens":0,
        "cached_input_tokens":0,"cache_write_input_tokens":0},"raw":{"output":[]}}}))
        .unwrap();
    assert!(e.observed_priced);
    assert!((e.observed_cost - input as f64 * 2.0 / 1e6).abs() < 1e-12);
    assert!(!e.priced);
    assert_eq!(e.coverage.unknown_cache_requests, 1);
}
