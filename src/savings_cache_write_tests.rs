// Native report/web adapters must not hide two equally invented cache costs
// behind zero savings. Here native and local totals are explicitly equal; this
// does not claim arbitrary native totals can be cast into local prompt units.
#[test]
fn no_op_receipt_parity_native_report_uses_reported_partitions() {
    let mut trajectory = Trajectory::with_model("gpt-6.1-sol".into());
    trajectory.record(&json!({"event":"run_started","data":{
        "model":"gpt-6.1-sol", "cache_ttl_seconds":1800,
        "implicit_cache_minimum_prefix_tokens":1024}}));
    let mut body = json!({"model":"gpt-6.1-sol", "input":[{
        "role":"user", "content":"unchanged native report prefix ".repeat(1000)}]});
    let mut expected = 0.0;
    for index in 0..3 {
        if index > 0 {
            body["input"].as_array_mut().unwrap().push(json!({
                "role":"user", "content":format!("report append {index}")
            }));
        }
        let input = prompt_projection(&body).unwrap().2;
        assert!((1024..100_000).contains(&input));
        let cached = if index == 1 { input / 2 } else { 0 };
        trajectory.record(&json!({"event":"model_request","run_id":"native-parity",
            "timestamp_ms":100 + index * 2,"data":{"step":index,
            "transport":{"api_base":"https://api.openai.com/v1"}, "request":body}}));
        let estimate = trajectory
            .record(&json!({"event":"model_response","run_id":"native-parity",
            "timestamp_ms":101 + index * 2,"data":{"step":index,"response_retries":0,
            "usage":{"input_tokens":input,"cached_input_tokens":cached,
            "cache_write_input_tokens":0,"output_tokens":100},"raw":{"output":[]}}}))
            .unwrap();
        expected += ((input - cached) as f64 * 2.0 + cached as f64 * 0.1 + 100.0 * 10.0) / 1e6;
        assert!(estimate.priced && estimate.observed_priced);
        assert_eq!(estimate.initial_compactions, 0);
        assert_eq!(estimate.pi_compactions, 0);
        assert_eq!(estimate.summary_cost, 0.0);
        assert!((estimate.observed_cost - expected).abs() < 1e-12);
        assert!(
            (estimate.carry_cost - expected).abs() < 1e-12,
            "NO_OP_NATIVE_PARITY: no unreported write/hit in an unchanged native primary"
        );
        assert!((estimate.pi_cost - expected).abs() < 1e-12);
        assert!(estimate.savings.abs() < 1e-12);
    }
}

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

#[test]
fn cache_write191_native_equivalent_defaults_reuse_each_divergent_branch() {
    for restored in [0_u64, 700] {
        let mut t = Trajectory::with_model("gpt-6.1-sol".into());
        t.record(&json!({"event":"run_started","data":{"model":"gpt-6.1-sol", "cache_ttl_seconds":1800, "implicit_cache_minimum_prefix_tokens":1024}}));
        let mut body = json!({"model":"gpt-6.1-sol","prompt_cache_key":"stable", "input":[{"role":"user","content":"prefix ".repeat(2000)}]});
        t.record(&json!({"event":"context_compacted","data":{"compaction_token_delta":restored}}));
        let mut carry_expected = 0.0;
        let mut pi_expected = 0.0;
        let mut previous = 0;
        for (index, options) in [
            None,
            Some(json!({})),
            Some(json!({"mode":"implicit","ttl":"30m"})),
            None,
        ]
        .into_iter()
        .enumerate()
        {
            if index == 2 {
                body["input"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"role":"user","content":"append ".repeat(100)}));
            }
            body.as_object_mut().unwrap().remove("prompt_cache_options");
            if let Some(options) = options {
                body["prompt_cache_options"] = options;
            }
            let input = prompt_projection(&body).unwrap().2;
            let cached = previous.min(input);
            let pi_cached = if index == 0 { 0 } else { previous + restored };
            carry_expected += (cached as f64 * 0.1 + (input - cached) as f64 * 2.5) / 1e6;
            pi_expected +=
                (pi_cached as f64 * 0.1 + (input + restored - pi_cached) as f64 * 2.5) / 1e6;
            t.record(&json!({"event":"model_request","timestamp_ms":100+index,"data":{"step":index,"route":"official","request":body}}));
            let e = t.record(&json!({"event":"model_response","timestamp_ms":100+index,"data":{"step":index,"usage":{"input_tokens":input,"cached_input_tokens":cached,"cache_write_input_tokens":input-cached,"output_tokens":0},"raw":{"output":[]}}})).unwrap();
            assert!(e.priced && e.observed_priced);
            assert_eq!(e.pi_compactions, 0);
            assert!(
                (e.carry_cost - carry_expected).abs() < 1e-12,
                "CACHE_WRITE_191_DEFAULT: equivalent options must preserve absolute warm price"
            );
            assert!((e.pi_cost - pi_expected).abs() < 1e-12);
            assert!((e.savings - (pi_expected - carry_expected)).abs() < 1e-12);
            previous = input;
        }
    }
}

#[test]
fn cache_write191_native_unsupported_lifetime_is_not_observed_30m_cost() {
    let mut body =
        json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"prefix ".repeat(2000)}]});
    let input = prompt_projection(&body).unwrap().2;
    let mut t = Trajectory::with_model("gpt-6.1-sol".into());
    let mut expected = 0.0;
    for (index, (ttl, written)) in [("30m", input), ("12h", input), ("12h", 0), ("30m", input)]
        .into_iter()
        .enumerate()
    {
        body["prompt_cache_options"] = json!({"mode":"implicit","ttl":ttl});
        t.record(&json!({"event":"model_request","data":{"step":index,"request":body}}));
        let e = t.record(&json!({"event":"model_response","data":{"step":index,"usage":{"input_tokens":input,"cached_input_tokens":0,"cache_write_input_tokens":written,"output_tokens":0},"raw":{"output":[]}}})).unwrap();
        if ttl == "30m" || written == 0 {
            expected += input as f64 * if written == 0 { 2.0 } else { 2.5 } / 1e6;
        }
        assert!(
            (e.observed_cost - expected).abs() < 1e-12,
            "CACHE_WRITE_191_NATIVE_TTL: unsupported writes remain unknown"
        );
        assert_eq!(e.coverage.unpriced_responses, usize::from(index > 0));
    }
}

#[test]
fn cache_write191_native_breakpoint_override_and_mixed_lifetimes_stay_unknown() {
    let mut t = Trajectory::with_model("gpt-6.1-sol".into());
    let mut expected = 0.0;
    for (index, (lifetimes, writes)) in [
        (vec!["30m"], true),
        (vec!["12h"], true),
        (vec!["30m", "12h"], true),
        (vec!["12h"], false),
        (vec!["30m"], true),
    ]
    .into_iter()
    .enumerate()
    {
        let body = json!({"model":"gpt-6.1-sol",
            "prompt_cache_options":{"mode":"explicit","ttl":"30m"},
            "input":[{"role":"user","content":lifetimes.iter().map(|ttl|
                json!({"type":"input_text","text":"prefix ".repeat(2000),
                    "prompt_cache_breakpoint":{"mode":"explicit","ttl":ttl}})
            ).collect::<Vec<_>>()}]});
        let input = prompt_projection(&body).unwrap().2;
        let written = if writes { input } else { 0 };
        t.record(&json!({"event":"model_request","data":{"step":index,"request":body}}));
        let e = t
            .record(&json!({"event":"model_response","data":{"step":index,
            "usage":{"input_tokens":input,"cached_input_tokens":0,
                "cache_write_input_tokens":written,"output_tokens":0},"raw":{"output":[]}}}))
            .unwrap();
        if !lifetimes.contains(&"12h") || !writes {
            expected += input as f64 * if writes { 2.5 } else { 2.0 } / 1e6;
        }
        assert!(
            (e.observed_cost - expected).abs() < 1e-12,
            "CACHE_WRITE_191_NATIVE_BREAKPOINT: unsupported override writes remain unknown"
        );
        assert_eq!(e.coverage.unpriced_responses, index.min(2));
    }
}

#[test]
fn cache_write191_native_typed_tool_output_keeps_unsupported_writes_unknown() {
    let mut t = Trajectory::with_model("gpt-6.1-sol".into());
    let mut expected = 0.0;
    for (index, (ttl, writes)) in [("30m", true), ("12h", true), ("12h", false), ("30m", true)]
        .into_iter()
        .enumerate()
    {
        let body = json!({"model":"gpt-6.1-sol",
            "prompt_cache_options":{"mode":"explicit","ttl":"30m"},
            "input":[{"type":"function_call","call_id":"call_191","name":"lookup","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_191","output":[
                    {"type":"input_text","text":"tool result ".repeat(2000),
                        "prompt_cache_breakpoint":{"mode":"explicit","ttl":ttl}}]}]});
        let input = prompt_projection(&body).unwrap().2;
        t.record(&json!({"event":"model_request","data":{"step":index,"request":body}}));
        let e = t
            .record(&json!({"event":"model_response","data":{"step":index,
            "usage":{"input_tokens":input,"cached_input_tokens":0,
                "cache_write_input_tokens":if writes {input} else {0},"output_tokens":0},
            "raw":{"output":[]}}}))
            .unwrap();
        if ttl == "30m" || !writes {
            expected += input as f64 * if writes { 2.5 } else { 2.0 } / 1e6;
        }
        assert!(
            (e.observed_cost - expected).abs() < 1e-12,
            "CACHE_WRITE_191_NATIVE_TOOL_OUTPUT: unsupported typed result writes remain unknown"
        );
        assert_eq!(e.coverage.unpriced_responses, usize::from(index > 0));
    }
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
