// Tests-first native/report tracer. Production native timing remains unchanged
// until this executes its intended original-write-deadline failure in hosted CI.
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
            "data":{"step":step,"usage":{"input_tokens":input,"output_tokens":0,
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
