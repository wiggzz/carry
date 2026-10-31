// Source-bound intended failures executed in CI41092267145 before the native
// adapter switched to the shared modeled-write owner. Keep these bodies intact.
fn native_modeled_write_deadline(change_lifetime: bool) {
    let mut t = Trajectory::with_model("gpt-6.1-sol".into());
    t.record(&json!({"event":"run_started","data":{
        "model":"gpt-6.1-sol","cache_ttl_seconds":7,
        "implicit_cache_minimum_prefix_tokens":1024}}));
    t.record(&json!({"event":"context_compacted","data":{"compaction_token_delta":0}}));
    let request = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
        "input":[{"role":"user","content":"synthetic lifetime prefix ".repeat(1000)}]});
    let input = prompt_projection(&request).unwrap().2;
    assert!(input > 1024);
    for (step, at) in [100, 106, 107].into_iter().enumerate() {
        if change_lifetime && step == 1 {
            t.record(&json!({"event":"session_resumed","data":{
                "model":"gpt-6.1-sol","cache_ttl_seconds":100,
                "implicit_cache_minimum_prefix_tokens":1024}}));
        }
        let before = t.snapshot();
        t.record(&json!({"event":"model_request","timestamp_ms":at*1000,
            "data":{"step":step,"route":"synthetic-lifetime","request":request}}));
        let after = t
            .record(&json!({"event":"model_response","timestamp_ms":at*1000,
            "data":{"usage_source":"normalized","step":step,"usage":{"input_tokens":input,"output_tokens":0,
                "cached_input_tokens":0,"cache_write_input_tokens":0},"raw":{"output":[]}}}))
            .unwrap();
        assert!(after.priced && after.observed_priced);
        assert_eq!(after.initial_compactions, 0);
        assert_eq!(after.pi_compactions, 0);
        assert!(
            (after.observed_cost - before.observed_cost - input as f64 * 2.0 / 1e6).abs() < 1e-12
        );
        let cached = if step == 1 { input } else { 0 };
        let expected =
            scenario_input_cost("gpt-6.1-sol", input, cached, Some(1024), estimated_cost_usd)
                .unwrap();
        assert!(
            (after.pi_cost - before.pi_cost - expected).abs() < 1e-12,
            "NATIVE_ORIGINAL_WRITE_DEADLINE: read/new policy cannot extend the first write"
        );
    }
}

#[test]
fn native_report_modeled_reads_expire_at_original_write_deadline() {
    native_modeled_write_deadline(false);
}

#[test]
fn native_report_new_policy_does_not_extend_original_write_deadline() {
    native_modeled_write_deadline(true);
}

#[test]
fn native_modeled_append_lifetime_and_replay_use_original_prefix_deadline() {
    // Rebuilding from the immutable event prefix is native report restart;
    // it must produce the same money and write deadlines as incremental replay.
    let mut events = vec![
        json!({"event":"run_started","data":{"model":"gpt-6.1-sol",
            "cache_ttl_seconds":7,"implicit_cache_minimum_prefix_tokens":1024}}),
        json!({"event":"context_compacted","data":{"compaction_token_delta":0}}),
    ];
    let mut request = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
        "input":[{"role":"user","content":"synthetic lifetime prefix ".repeat(1000)}]});
    let first_input = prompt_projection(&request).unwrap().2;
    let mut t = Trajectory::default();
    for event in &events {
        t.record(event);
    }
    for (step, at) in [100, 106, 107, 108].into_iter().enumerate() {
        if step == 1 {
            let policy = json!({"event":"session_resumed","data":{
                "model":"gpt-6.1-sol","cache_ttl_seconds":100,
                "implicit_cache_minimum_prefix_tokens":1024}});
            t.record(&policy);
            events.push(policy);
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":"append"}));
        }
        let input = prompt_projection(&request).unwrap().2;
        let before = t.snapshot();
        let req = json!({"event":"model_request","timestamp_ms":at*1000,
            "data":{"step":step,"route":"synthetic-lifetime","request":request}});
        let res = json!({"event":"model_response","timestamp_ms":at*1000+500,
            "data":{"usage_source":"normalized","step":step,"usage":{"input_tokens":input,"output_tokens":0,
            "cached_input_tokens":0,"cache_write_input_tokens":0},"raw":{"output":[]}}});
        t.record(&req);
        t.record(&res);
        events.extend([req, res]);
        let after = t.snapshot();
        assert!(after.priced && after.observed_priced);
        assert_eq!(after.initial_compactions, 0);
        assert_eq!(after.pi_compactions, 0);
        assert!(
            (after.observed_cost - before.observed_cost - input as f64 * 2.0 / 1e6).abs() < 1e-12
        );
        let cached = match step {
            1 => first_input,
            3 => input,
            _ => 0,
        };
        let expected =
            scenario_input_cost("gpt-6.1-sol", input, cached, Some(1024), estimated_cost_usd)
                .unwrap();
        assert!((after.pi_cost - before.pi_cost - expected).abs() < 1e-12);
        let mut replay = Trajectory::default();
        for event in &events {
            replay.record(event);
        }
        assert_eq!(
            serde_json::to_value(replay.snapshot()).unwrap(),
            serde_json::to_value(after).unwrap()
        );
    }
}

#[test]
fn native_missing_write_clock_cannot_borrow_native_hits_or_response_time() {
    let mut t = Trajectory::with_model("gpt-6.1-sol".into());
    t.record(&json!({"event":"context_compacted","data":{"compaction_token_delta":0}}));
    let request = json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"unknown clock ".repeat(1000)}]});
    let input = prompt_projection(&request).unwrap().2;
    for step in 0..2 {
        let before = t.snapshot();
        t.record(&json!({"event":"model_request","data":{"step":step,"route":"synthetic-lifetime","request":request}}));
        let after = t
            .record(&json!({"event":"model_response","timestamp_ms":100000+step,
            "data":{"usage_source":"normalized","step":step,"usage":{"input_tokens":input,"output_tokens":0,
            "cached_input_tokens":input,"cache_write_input_tokens":0},"raw":{"output":[]}}}))
            .unwrap();
        assert!(after.observed_priced);
        assert_eq!(after.coverage.unknown_expiry_requests, step as usize + 1);
        assert!(
            (after.observed_cost - before.observed_cost - input as f64 * 0.1 / 1e6).abs() < 1e-12
        );
        let cold =
            scenario_input_cost("gpt-6.1-sol", input, 0, Some(1024), estimated_cost_usd).unwrap();
        assert!((after.pi_cost - before.pi_cost - cold).abs() < 1e-12);
    }
}
