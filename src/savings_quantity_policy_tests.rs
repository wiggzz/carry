// Decision policy is quantity-only; each adapter keeps its own measurement and
// history validation. Monetary coverage never enters these decisions.
#[test]
fn quantity_policy_decision_table() {
    let scenario = Scenario {
        context_window: 40_000,
        reserve: 10_000,
        keep_recent: 20_000,
        summary: 1_000,
    };
    for (recognized, input, protected, output, capacity, expected) in [
        (false, 30_000, 0, 1, None, Ok(None)),
        (false, 30_001, 0, 1, None, Ok(Some(20_000))),
        (true, 35_000, 0, 1, None, Ok(None)),
        (false, 35_000, 25_000, 1, None, Ok(Some(25_000))),
        (false, 35_000, 30_000, 1, None, Err(())),
        (false, 25_000, 0, 1, Some(24_000), Ok(Some(20_000))),
        (false, 25_000, 0, 20_000, None, Err(())),
    ] {
        assert_eq!(
            scenario.initial_compaction(recognized, input, protected, output, capacity),
            expected
        );
    }
    for (input, output, fixed, expected) in [
        (30_000, 0, 0, Ok(None)),
        (30_000, 1, 0, Ok(Some(20_000))),
        (35_000, 0, 25_000, Ok(Some(25_000))),
        (35_000, 0, 30_000, Err(())),
    ] {
        assert_eq!(scenario.response_compaction(input, output, fixed), expected);
    }
}

#[test]
fn quantity_policy_real_coordinators_preserve_summary_after_unknown_money() {
    for ttl in ["30m", "12h"] {
        let scenario = Scenario {
            context_window: 40_000,
            ..Scenario::default()
        };
        let mut local = Trajectory::with_scenario(scenario);
        let mut native = crate::proxy_savings::Trajectory::default();
        native.scenario = scenario;
        local.record(&json!({"event":"run_started","data":{
            "model":"gpt-6.1-sol","context_window":40_000,
            "cache_ttl_seconds":1800,"implicit_cache_minimum_prefix_tokens":1024
        }}));
        let mut body = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
            "prompt_cache_options":{"mode":"implicit","ttl":ttl},
            "input":[{"role":"assistant","content":" evidence".repeat(10_000)},
                     {"role":"user","content":"continue"}]});
        for step in 1..=3 {
            if step == 2 {
                body["input"].as_array_mut().unwrap().push(json!({
                    "role":"assistant","content":" growth".repeat(25_000)
                }));
            } else if step == 3 {
                body["input"].as_array_mut().unwrap().push(json!({
                    "role":"user","content":" append".repeat(100)
                }));
            }
            // Explicitly equate the incoming totals for this differential, not
            // a claim that provider totals generally equal a local tokenizer.
            let input = prompt_projection(&body).unwrap().2;
            let writes = u64::from(step == 2) * 1000;
            let response = json!({"output":[],"usage":{
                "input_tokens":input,"output_tokens":0,
                "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":writes}
            }});
            native.primary(&body, &response, "synthetic", step, true);
            local.record(&json!({"event":"model_request","timestamp_ms":step*1000,
                "data":{"step":step,"route":"synthetic","request":body}}));
            local.record(&json!({"event":"model_response","data":{"step":step,
                "usage":{"input_tokens":input,"output_tokens":0,"cached_input_tokens":0,
                         "cache_write_input_tokens":writes},"raw":{"output":[]}}}));
            let a = native.snapshot();
            let b = local.snapshot();
            assert_eq!(a.initial_compactions, 0);
            assert_eq!(b.initial_compactions, 0);
            assert_eq!(a.traditional_compactions, u64::from(step >= 2));
            assert_eq!(b.pi_compactions, usize::from(step >= 2), "{ttl}/{step}");
            assert!((a.observed_primary_cost - b.observed_cost).abs() < 1e-12);
            assert_eq!(
                a.unavailable_primary_cost_calls,
                u64::from(ttl == "12h" && step >= 2)
            );
            assert_eq!(
                b.coverage.unpriced_responses,
                usize::from(ttl == "12h" && step >= 2)
            );
            assert_eq!(a.summary_cost > 0.0, ttl == "30m" && step >= 2);
            assert_eq!(b.summary_cost > 0.0, ttl == "30m" && step >= 2);
        }
        // Current-format native restart cannot repeat the successful summary.
        let frozen = serde_json::to_value(&native).unwrap();
        let reloaded: crate::proxy_savings::Trajectory =
            serde_json::from_value(frozen.clone()).unwrap();
        assert_eq!(serde_json::to_value(reloaded).unwrap(), frozen);
        assert_eq!(local.last_output_proxy, token_units(&json!([])));
        assert_eq!(local.result.pi_compactions, 1);
        assert_eq!(local.summary, scenario.summary);
        assert_eq!(local.snapshot().priced, ttl == "30m");
    }
}
