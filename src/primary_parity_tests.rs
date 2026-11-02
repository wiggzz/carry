// Differential replay through both real coordinators. Quantity adapters remain
// distinct: only this synthetic fixture deliberately equates their quantities.

// Review5714176642: changing forecast policy cannot rename summarized history.
// Tests-first lifetime tracer. No implicit provider read-refresh contract is
// declared: reading at t=106 must not move the modeled t=100 write deadline.
#[test]
fn modeled_cache_read_does_not_extend_original_write_lifetime_across_restart() {
    use carry::models::{Route, resolve_model};
    let mut resolved = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    resolved.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 7;
    let mut t = Trajectory::default();
    // Start with a known cold shared receipt, then diverge the next request.
    let request = body();
    for (index, (at, cached)) in [(100, 0), (106, 2000), (107, 0)].into_iter().enumerate() {
        if index == 1 { t.projected(100, 100, true); }
        let before = t.snapshot();
        t.primary_resolved(
            &request,
            &terminal(2000, cached, 2000 - cached, 0),
            "lifetime-fixture",
            at,
            true,
            Some(&resolved),
        );
        let after = t.snapshot();
        assert!(after.priced);
        assert_eq!(after.excluded_unpriced_calls, 0);
        assert_eq!(after.matched_primary_responses, index as u64 + 1);
        assert_eq!(after.initial_compactions, 0);
        assert_eq!(after.traditional_compactions, 0);
        assert_eq!(after.summary_cost, 0.0);
        let actual = (2000 - cached) as f64 * 2.5 / 1e6 + cached as f64 * 0.1 / 1e6;
        assert!(
            (after.observed_primary_cost - before.observed_primary_cost - actual).abs() < 1e-12
        );
        let forecast =
            scenario_input_cost_resolved("gpt-6.1-sol", 2000, cached, Some(1024), Some(&resolved))
                .unwrap();
        assert!(
            (after.traditional_cost - before.traditional_cost - forecast).abs() < 1e-12,
            "ORIGINAL_WRITE_DEADLINE: a non-refreshing read cannot create another write"
        );
        t = serde_json::from_value(state(&t)).unwrap();
    }
}

#[test]
fn modeled_cache_append_and_new_policy_cannot_extend_old_prefix_deadline() {
    use carry::models::{Route, resolve_model};
    let mut resolved = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    resolved.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 7;
    let mut t = Trajectory::default();
    // Initial actual cache state is known; only later requests diverge.
    let mut request = body();
    for (index, (at, input, cached)) in [(100, 2000, 0), (106, 2100, 2000), (107, 2100, 0)]
        .into_iter()
        .enumerate()
    {
        if index == 1 {
            t.projected(100, 100, true);
            // New lifetime applies only to newly written extents. The original
            // prefix still expires at107, including across serialization.
            resolved.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 100;
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":"append"}));
        }
        let before = t.snapshot();
        t.primary_resolved(
            &request,
            &terminal(input, 0, 0, 0),
            "lifetime-fixture",
            at,
            true,
            Some(&resolved),
        );
        let after = t.snapshot();
        assert_eq!(after.excluded_unpriced_calls, 0);
        assert_eq!(after.initial_compactions, 0);
        assert_eq!(after.traditional_compactions, 0);
        let forecast =
            scenario_input_cost_resolved("gpt-6.1-sol", input, cached, Some(1024), Some(&resolved))
                .unwrap();
        let forecast = if index == 0 { input as f64 * 2.0 / 1e6 } else { forecast };
        assert!((after.traditional_cost - before.traditional_cost - forecast).abs() < 1e-12);
        assert!(
            (after.observed_primary_cost - before.observed_primary_cost - input as f64 * 2.0 / 1e6)
                .abs()
                < 1e-12
        );
        t = serde_json::from_value(state(&t)).unwrap();
    }
}

// Review5715769560 tests-first: live cache namespace production deliberately
// unchanged until this exact-source differential reaches the intended failure.
#[test]
fn modeled_cache_equivalent_options_keep_warmth_in_both_quantity_adapters() {
    let mut live = Trajectory::default();
    let mut native = crate::savings::Trajectory::with_model("gpt-6.1-sol".into());
    live.projected(100, 100, true);
    native.record(&json!({"event":"run_started","data":{"model":"gpt-6.1-sol",
        "cache_ttl_seconds":1800,"implicit_cache_minimum_prefix_tokens":1024}}));
    native.record(&json!({"event":"context_compacted","data":{"compaction_token_delta":0}}));
    let mut request = json!({"model":"gpt-6.1-sol","max_output_tokens":1,"prompt_cache_key":"synthetic-semantic-scope",
        "input":[{"role":"user","content":"synthetic semantic cache prefix ".repeat(1000)}]});
    let input = crate::savings::prompt_projection(&request).unwrap().2;
    assert!(input > 1024);
    for (index, options) in [
        None,
        Some(json!({})),
        Some(json!({"mode":"implicit","ttl":"30m"})),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        request
            .as_object_mut()
            .unwrap()
            .remove("prompt_cache_options");
        if let Some(options) = options {
            request["prompt_cache_options"] = options;
        }
        live = serde_json::from_value(state(&live)).unwrap();
        let before = live.snapshot();
        let native_before = native.snapshot();
        live.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "semantic-scope",
            100 + index as u64,
            true,
        );
        native.record(
            &json!({"event":"model_request","timestamp_ms":(100+index)*1000,
            "data":{"step":index,"route":"semantic-scope","request":request}}),
        );
        let n = native
            .record(
                &json!({"event":"model_response","timestamp_ms":(100+index)*1000,
            "data":{"usage_source":"normalized","step":index,"usage":{"input_tokens":input,"output_tokens":0,
            "cached_input_tokens":0,"cache_write_input_tokens":0},"raw":{"output":[]}}}),
            )
            .unwrap();
        let after = live.snapshot();
        assert!(after.priced && n.priced && n.observed_priced);
        assert_eq!(after.excluded_unpriced_calls, 0);
        assert_eq!(after.initial_compactions, 0);
        assert_eq!(after.traditional_compactions, 0);
        assert_eq!(after.summary_cost, 0.0);
        let actual = input as f64 * 2.0 / 1e6;
        assert!(
            (after.observed_primary_cost - before.observed_primary_cost - actual).abs() < 1e-12
        );
        assert!((n.observed_cost - native_before.observed_cost - actual).abs() < 1e-12);
        let expected = input as f64 * 0.1 / 1e6;
        assert!((n.pi_cost - native_before.pi_cost - expected).abs() < 1e-12);
        assert!(
            (after.traditional_cost - before.traditional_cost - expected).abs() < 1e-12,
            "SEMANTIC_CACHE_SCOPE: equivalent omitted/empty/default options cannot rename a live write"
        );
        assert!((after.traditional_cost - n.pi_cost).abs() < 1e-12);
    }
}

// Review5716439062: exercise the real consumers past the 128-event retention
// boundary, not just the container's serialized length. Both histories really
// diverge after the initial known-cold request; observed receipts stay independent.
fn modeled_write_pressure_costs(native_adapter: bool) {
    let mut live = Trajectory::default();
    let mut events = vec![
        json!({"event":"run_started","data":{"model":"gpt-6.1-sol",
            "cache_ttl_seconds":1800,"implicit_cache_minimum_prefix_tokens":1024}}),
    ];
    let mut native = crate::savings::Trajectory::default();
    for event in &events {
        native.record(event);
    }
    let mut request = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
        "prompt_cache_key":"synthetic-pressure",
        "input":[{"role":"user","content":"synthetic retained prefix ".repeat(400)}]});
    let mut previous_input = 0;
    let mut mismatches = Vec::new();
    for step in 0..132 {
        if step == 1 {
            live.projected(100, 100, true);
            let edit = json!({"event":"context_compacted","data":{"compaction_token_delta":0}});
            native.record(&edit);
            events.push(edit);
        }
        if step > 0 {
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("append {step}")}));
        }
        let input = crate::savings::prompt_projection(&request).unwrap().2;
        assert!(input > 1024 && input > previous_input);
        live = serde_json::from_value(state(&live)).unwrap();
        let before = live.snapshot();
        let native_before = native.snapshot();
        live.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "synthetic-pressure",
            100,
            true,
        );
        let req = json!({"event":"model_request","timestamp_ms":100000,
            "data":{"step":step,"route":"synthetic-pressure","request":request}});
        let res = json!({"event":"model_response","timestamp_ms":100000,
            "data":{"usage_source":"normalized","step":step,"usage":{"input_tokens":input,"output_tokens":0,
                "cached_input_tokens":0,"cache_write_input_tokens":0},"raw":{"output":[]}}});
        native.record(&req);
        native.record(&res);
        events.extend([req, res]);
        let after = live.snapshot();
        let n = native.snapshot();
        assert!(after.priced && n.priced && n.observed_priced);
        assert_eq!(after.excluded_unpriced_calls, 0);
        assert_eq!(after.matched_primary_responses, step + 1);
        assert_eq!(n.coverage.responses, step as usize + 1);
        assert_eq!(after.initial_compactions, 0);
        assert_eq!(after.traditional_compactions, 0);
        assert_eq!(n.initial_compactions, 0);
        assert_eq!(n.pi_compactions, 0);
        assert_eq!(after.summary_cost, 0.0);
        assert_eq!(n.summary_cost, 0.0);
        assert_eq!(live.history.last_traditional_input, input);
        let actual = input as f64 * 2.0 / 1e6;
        assert!(
            (after.observed_primary_cost - before.observed_primary_cost - actual).abs() < 1e-12
        );
        assert!((after.carry_cost - before.carry_cost - actual).abs() < 1e-12);
        assert!((n.observed_cost - native_before.observed_cost - actual).abs() < 1e-12);
        assert!((n.carry_cost - native_before.carry_cost - actual).abs() < 1e-12);
        // Every write is at100 with lifetime1800. No expiry, native hit, cache
        // policy change or summary can justify making this prefix cold.
        let expected = if step == 0 { input as f64 * 2.0 / 1e6 } else {
            (previous_input as f64 * 0.1 + (input - previous_input) as f64 * 2.5) / 1e6
        };
        let delta = if native_adapter {
            n.pi_cost - native_before.pi_cost
        } else {
            after.traditional_cost - before.traditional_cost
        };
        if (delta - expected).abs() >= 1e-12 {
            mismatches.push((step, input, expected, delta));
        }
        // Replaying the immutable native event prefix is its restart boundary.
        // Do it after the cap-triggering append and before the next priced call.
        if step == 128 {
            let mut replay = crate::savings::Trajectory::default();
            for event in &events {
                replay.record(event);
            }
            assert_eq!(
                serde_json::to_value(replay.snapshot()).unwrap(),
                serde_json::to_value(n).unwrap()
            );
            native = replay;
        }
        previous_input = input;
    }
    assert!(
        mismatches.is_empty(),
        "CACHE_RETENTION_MONEY: numeric eviction cannot manufacture a cold charge; native={native_adapter}, mismatches={mismatches:?}"
    );
}

#[test]
fn modeled_write_pressure_keeps_live_money_across_restart() {
    modeled_write_pressure_costs(false);
}

#[test]
fn modeled_write_pressure_keeps_native_report_money_across_replay() {
    modeled_write_pressure_costs(true);
}

// Unlike the coalescible pressure lifecycle above, these writes have different
// clocks. The bounded owner may lose suffix precision, never invent cold money
// or reset independent history. Each real adapter has its own registration.
fn modeled_retention_unknown_costs(native_adapter: bool) {
    let mut live = Trajectory::default();
    let mut native = crate::savings::Trajectory::with_model("gpt-6.1-sol".into());
    native.record(&json!({"event":"run_started","data":{"model":"gpt-6.1-sol",
        "cache_ttl_seconds":1800,"implicit_cache_minimum_prefix_tokens":1024}}));
    let mut request = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
        "input":[{"role":"user","content":"synthetic different-clock prefix ".repeat(400)}]});
    let mut paid = 0.0;
    let mut forecast = 0.0;
    let mut previous = 0;
    for step in 0..132_u64 {
        if step == 1 {
            live.projected(100, 100, true);
            native.record(&json!({"event":"context_compacted","data":{"compaction_token_delta":0}}));
        }
        if step > 0 {
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("append {step}")}));
        }
        let input = crate::savings::prompt_projection(&request).unwrap().2;
        assert!(input > 1024 && input > previous);
        live = serde_json::from_value(state(&live)).unwrap();
        live.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "retention-unknown",
            100 + step,
            true,
        );
        native.record(
            &json!({"event":"model_request","timestamp_ms":(100+step)*1000,
            "data":{"step":step,"route":"retention-unknown","request":request}}),
        );
        native.record(
            &json!({"event":"model_response","timestamp_ms":(100+step)*1000,
            "data":{"usage_source":"normalized","step":step,"usage":{"input_tokens":input,"output_tokens":0,
                "cached_input_tokens":0,"cache_write_input_tokens":0},"raw":{"output":[]}}}),
        );
        paid += input as f64 * 2.0 / 1e6;
        if step <= 128 {
            forecast += if step == 0 { input as f64 * 2.0 / 1e6 } else {
                (previous as f64 * 0.1 + (input - previous) as f64 * 2.5) / 1e6
            };
        }
        let l = live.snapshot();
        let n = native.snapshot();
        // Exact actual expense remains known even when the alternative does not.
        assert!((l.observed_primary_cost - paid).abs() < 1e-12);
        assert!((n.observed_cost - paid).abs() < 1e-12);
        assert_eq!(l.coverage.primary_responses, step + 1);
        assert_eq!(n.coverage.responses, step as usize + 1);
        assert_eq!(l.initial_compactions, 0);
        assert_eq!(l.traditional_compactions, 0);
        assert_eq!(n.initial_compactions, 0);
        assert_eq!(n.pi_compactions, 0);
        assert_eq!(l.summary_cost, 0.0);
        assert_eq!(n.summary_cost, 0.0);
        assert_eq!(live.history.last_traditional_input, input);
        assert_eq!(l.matched_primary_responses, (step + 1).min(129));
        assert_eq!(l.excluded_unpriced_calls, (step + 1).saturating_sub(129));
        if native_adapter {
            assert!(
                (n.pi_cost - forecast).abs() < 1e-12,
                "RETENTION_UNKNOWN_NATIVE"
            );
            assert_eq!(n.priced, step <= 128);
            assert_eq!(
                n.coverage.unknown_cache_requests,
                (step + 1).saturating_sub(129) as usize
            );
        } else {
            assert!(
                (l.traditional_cost - forecast).abs() < 1e-12,
                "RETENTION_UNKNOWN_LIVE"
            );
            assert_eq!(
                l.coverage.unknown_cache_requests,
                (step + 1).saturating_sub(129)
            );
            assert!(state(&live)["history_continuity"].is_object());
        }
        previous = input;
    }
}

#[test]
fn modeled_retention_unknown_preserves_live_money_and_history_after_restart() {
    modeled_retention_unknown_costs(false);
}

#[test]
fn modeled_retention_unknown_preserves_native_money_and_quantity_history() {
    modeled_retention_unknown_costs(true);
}

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
                    "data":{"usage_source":"normalized","step":index,"usage":{"input_tokens":input,"output_tokens":7,
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
