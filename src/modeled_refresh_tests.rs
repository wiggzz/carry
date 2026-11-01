// Conditional API-equivalent branch coverage, NOT observed provider residency.
// Both real coordinators receive independent zero-write native receipts. A read
// at1799s into the modeled lifetime must preserve billing coverage beyond the
// original deadline without adding a paid write or changing historic money.
fn modeled_refresh_coordinator(report: bool) {
    use carry::models::{Route, ScenarioCachePolicy, resolve_model};
    let resolved = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://unknown.invalid/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let policy = ScenarioCachePolicy::for_model("gpt-6.1-sol");
    assert_eq!(policy.ttl_seconds, 1800);
    // This is explicitly a conditional API-equivalent scenario. Native route
    // entitlement is absent and cannot be borrowed by the observed lane.
    assert!(resolved.cache.is_none());
    let mut live = Trajectory::default();
    live.projected(100, 100, true);
    let request = json!({"model":"gpt-6.1-sol", "max_output_tokens":1,
        "prompt_cache_key":"synthetic-modeled-refresh",
        "input":[{"role":"user","content":"conditional modeled prefix ".repeat(700)}]});
    let input = crate::savings::prompt_projection(&request).unwrap().2;
    assert!(input > 1024 && input < 20000);
    let mut events = vec![
        json!({"event":"run_started","data":{"model":"gpt-6.1-sol",
            "scenario_cache_policy":policy}}),
        json!({"event":"context_compacted","data":{"compaction_token_delta":0}}),
    ];
    let mut original_writes = None;
    let mut mismatches = Vec::new();
    let mut changed_original_writes = Vec::new();
    for (step, (at, warm)) in [(100, false), (1899, true), (1901, true), (3701, false)]
        .into_iter()
        .enumerate()
    {
        live = serde_json::from_value(state(&live)).unwrap();
        // Rebuild the actual report coordinator from its original immutable
        // trace on each step, separately from live's current-format restart.
        let mut native = crate::savings::Trajectory::default();
        for event in &events {
            native.record(event);
        }
        let before = live.snapshot();
        let prior = native.snapshot();
        live.primary_resolved(
            &request,
            &terminal(input, 0, 0, 0),
            "modeled-refresh",
            at,
            true,
            Some(&resolved),
        );
        let req = json!({"event":"model_request","timestamp_ms":at*1000,
            "data":{"step":step,"route":"modeled-refresh","request":request}});
        let res = json!({"event":"model_response","timestamp_ms":at*1000,
            "data":{"usage_source":"normalized","step":step,
                "usage":{"input_tokens":input,"output_tokens":0,
                    "cached_input_tokens":0,"cache_write_input_tokens":0},
                "raw":{"output":[]}}});
        native.record(&req);
        let n = native.record(&res).unwrap();
        events.extend([req, res]);
        let after = live.snapshot();
        assert!(after.priced && n.priced && n.observed_priced);
        assert_eq!(after.excluded_unpriced_calls, 0);
        assert_eq!(after.matched_primary_responses, step as u64 + 1);
        assert_eq!(n.coverage.responses, step + 1);
        assert_eq!(after.initial_compactions, 0);
        assert_eq!(after.traditional_compactions, 0);
        assert_eq!(n.initial_compactions, 0);
        assert_eq!(n.pi_compactions, 0);
        assert_eq!(after.summary_cost, 0.0);
        assert_eq!(n.summary_cost, 0.0);
        let paid = input as f64 * 2.0 / 1e6;
        assert!((after.observed_primary_cost - before.observed_primary_cost - paid).abs() < 1e-12);
        assert!((n.observed_cost - prior.observed_cost - paid).abs() < 1e-12);
        assert!((after.carry_cost - before.carry_cost - paid).abs() < 1e-12);
        assert!((n.carry_cost - prior.carry_cost - paid).abs() < 1e-12);
        let writes = state(&live)["receipts"]["modeled_cache"]["writes"].clone();
        if step == 0 {
            original_writes = Some(writes.clone());
        }
        if (step == 1 || step == 2) && writes != *original_writes.as_ref().unwrap() {
            changed_original_writes.push(at);
        }
        let expected = input as f64 * if warm { 0.1 } else { 2.5 } / 1e6;
        let actual = if report {
            n.pi_cost - prior.pi_cost
        } else {
            after.traditional_cost - before.traditional_cost
        };
        if (actual - expected).abs() > 1e-12 {
            mismatches.push((at, actual, expected));
        }
    }
    assert!(
        mismatches.is_empty() && changed_original_writes.is_empty(),
        "MODELED_QUALIFIED_READ_REFRESH report={report}: prices={mismatches:?}, changed_original_writes={changed_original_writes:?}"
    );
}

#[test]
fn modeled_live_qualified_read_refresh_survives_original_expiry_and_restart() {
    modeled_refresh_coordinator(false);
}

#[test]
fn modeled_report_qualified_read_refresh_survives_original_expiry_and_replay() {
    modeled_refresh_coordinator(true);
}
