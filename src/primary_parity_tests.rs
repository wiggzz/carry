// Differential replay through both real coordinators. Quantity adapters remain
// distinct: only this synthetic fixture deliberately equates their quantities.

// Tests-first follow-up to review5714176642. History-contract production is
// intentionally unchanged until these intended native failures execute.
fn cache_options_preserve_summarized_growth(initialized: bool) {
    let mut t = Trajectory::default();
    let mut request = body();
    request["max_output_tokens"] = json!(1);
    if !initialized {
        t.primary(&request, &terminal(10000, 0, 0, 0), "official", 98, true);
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"grow"}));
    }
    t.primary(&request, &terminal(300000, 0, 0, 0), "official", 99, true);
    let before = t.snapshot();
    assert_eq!(before.initial_compactions, u64::from(initialized));
    assert_eq!(before.traditional_compactions, u64::from(!initialized));
    assert_eq!(t.history.retired, 280000);
    for (index, options) in [
        None,
        Some(json!({})),
        Some(json!({"mode":"implicit","ttl":"30m"})),
        Some(json!({"mode":"explicit","ttl":"30m"})),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        t = serde_json::from_value(state(&t)).unwrap();
        request
            .as_object_mut()
            .unwrap()
            .remove("prompt_cache_options");
        if let Some(options) = options {
            request["prompt_cache_options"] = options;
        }
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":format!("cache append {index}")}));
        let input = 300100 + index as u64 * 100;
        t.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "official",
            100 + index as u64,
            true,
        );
        let after = t.snapshot();
        assert_eq!(after.excluded_unpriced_calls, 0);
        assert_eq!(after.summary_cost, before.summary_cost);
        assert_eq!(
            after.traditional_compactions,
            before.traditional_compactions
        );
        assert_eq!(
            after.initial_compactions, before.initial_compactions,
            "CACHE_OPTIONS_HISTORY: forecast policy must not rename summarized history"
        );
        assert_eq!(t.history.retired, 280000);
        assert_eq!(t.history.summary, 1000);
        assert_eq!(t.history.last_traditional_input, 21100 + index as u64 * 100);
    }
}

#[test]
fn history_cache_options_preserve_free_initialized_growth() {
    cache_options_preserve_summarized_growth(true);
}

#[test]
fn history_cache_options_preserve_paid_summary_growth() {
    cache_options_preserve_summarized_growth(false);
}

#[test]
fn primary_price_owner_never_forecasts_an_identical_or_unknown_observation() {
    let mut history = crate::savings::PrimaryHistory::identical();
    assert_eq!(
        history.price(Some(0.003), || panic!("invented forecast")),
        Some((0.003, 0.003))
    );
    assert_eq!(history.price(None, || panic!("unknown is not zero")), None);
    history.diverge();
    assert_eq!(
        history.price(Some(0.003), || Some(0.007)),
        Some((0.003, 0.007))
    );
    let restored: crate::savings::PrimaryHistory =
        serde_json::from_str(&serde_json::to_string(&history).unwrap()).unwrap();
    assert_eq!(restored.price(Some(0.003), || None), None);
    assert_eq!(
        crate::savings::PrimaryHistory::default().price(Some(0.003), || None),
        None
    );
}

#[test]
fn native_and_proxy_share_noop_money_and_keep_divergence_after_later_noop() {
    for diverged in [false, true] {
        for native_extra in [0, 17000] {
            // A divergent local alternative is only compared numerically with
            // the native adapter when this fixture explicitly equates units.
            if diverged && native_extra != 0 {
                continue;
            }
            let mut proxy = Trajectory::default();
            let mut native = crate::savings::Trajectory::with_model("gpt-6.1-sol".into());
            native.record(&json!({"event":"run_started","data":{
                "model":"gpt-6.1-sol","cache_ttl_seconds":1800,
                "implicit_cache_minimum_prefix_tokens":1024}}));
            let mut request = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
                "input":[{"role":"user","content":"synthetic shared prefix ".repeat(1000)}]});
            for index in 0..3 {
                if index > 0 {
                    request["input"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"role":"user","content":format!("append {index}")}));
                }
                if diverged && index == 1 {
                    // Equal-token but different-content Carry edit, then an
                    // ordinary append/no-op. Last-plan equality is NOT proof.
                    proxy.projected(0, 0, true);
                    native.record(
                        &json!({"event":"context_compacted","data":{"compaction_token_delta":0}}),
                    );
                } else {
                    proxy.projected(0, 0, false);
                }
                let input = crate::savings::prompt_projection(&request).unwrap().2 + native_extra;
                let cached = if index == 1 { input / 2 } else { 0 };
                let written = if index == 0 { input } else { 0 };
                let receipt = terminal(input, cached, written, 7);
                proxy.primary(&request, &receipt, "official", 100 + index, true);
                native.record(
                    &json!({"event":"model_request","timestamp_ms":(100 + index)*1000,
                    "data":{"step":index,"route":"official","request":request}}),
                );
                let local = native.record(&json!({"event":"model_response","timestamp_ms":(100+index)*1000,
                    "data":{"step":index,"usage":{"input_tokens":input,"output_tokens":7,
                    "cached_input_tokens":cached,"cache_write_input_tokens":written},"raw":{"output":[]}}})).unwrap();
                let live = proxy.snapshot();
                assert!(local.priced && local.observed_priced && live.priced);
                assert_eq!(live.excluded_unpriced_calls, 0);
                assert!((local.observed_cost - live.observed_primary_cost).abs() < 1e-12);
                assert!((local.carry_cost - live.carry_cost).abs() < 1e-12);
                assert!(
                    (local.pi_cost - live.traditional_cost).abs() < 1e-12,
                    "both adapters must use the same observation-versus-forecast decision"
                );
                if !diverged || index == 0 {
                    assert!((local.pi_cost - local.observed_cost).abs() < 1e-12);
                } else {
                    assert!(
                        (local.pi_cost - local.observed_cost).abs() > 1e-6,
                        "a later no-op cannot copy a real receipt onto divergent history"
                    );
                }
                assert_eq!(live.initial_compactions, 0);
                assert_eq!(live.traditional_compactions, 0);
                proxy = serde_json::from_value(state(&proxy)).unwrap();
            }
        }
    }
}
