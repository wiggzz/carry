// Pricing availability is not a native-input continuity event. These tests
// intentionally precede the coordinator repair; rebuilt CI must establish RED.
fn history_through_tariff_gap(initialized: bool, unknown: bool) {
    use carry::models::{Rates, Route, resolve_model};
    let known = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let mut gap = known.clone();
    gap.rates = if unknown {
        None
    } else {
        Some(Rates {
            input: 0.0,
            cached: 0.0,
            write: 0.0,
            output: 0.0,
        })
    };
    // A known zero tariff is a priced observation, not an unknown tariff.
    let mut t = Trajectory::default();
    let mut request = body();
    request["max_output_tokens"] = json!(1);
    if !initialized {
        t.primary_resolved(
            &request,
            &terminal(10000, 0, 0, 0),
            "tariff-gap",
            99,
            true,
            Some(&known),
        );
        request["input"].as_array_mut().unwrap().push(json!({
            "role":"user", "content":"in-window growth"}));
    }
    for (step, input) in [300000, 300100, 300200].into_iter().enumerate() {
        if step > 0 {
            request["input"].as_array_mut().unwrap().push(json!({
                "role":"user", "content":format!("known append {step}")}));
        }
        t.primary_resolved(
            &request,
            &terminal(input, 0, 0, 0),
            "tariff-gap",
            100 + step as u64,
            true,
            Some(&known),
        );
    }
    t.reviewer_resolved(&request, &terminal(100, 0, 0, 10), true, Some(&known));
    let before = t.snapshot();
    assert_eq!(t.history.retired, 280000);
    assert_eq!(t.history.summary, 1000);
    assert_eq!(t.history.last_traditional_input, 21200);
    assert_eq!(before.initial_compactions, u64::from(initialized));
    assert_eq!(before.traditional_compactions, u64::from(!initialized));
    assert_eq!(before.summary_cost > 0.0, !initialized);
    assert!(before.reviewer_cost > 0.0);
    assert_eq!(before.unavailable_primary_cost_calls, 0);
    let mut differences = Vec::new();
    for (step, input) in [300300, 300400].into_iter().enumerate() {
        t = serde_json::from_value(state(&t)).unwrap();
        request["input"].as_array_mut().unwrap().push(json!({
            "role":"user", "content":format!("tariff append {step}")}));
        t.primary_resolved(
            &request,
            &terminal(input, 0, 0, 0),
            "tariff-gap",
            103 + step as u64,
            true,
            Some(if step == 0 { &gap } else { &known }),
        );
        let after = t.snapshot();
        // Run the paid-observation and cumulative-boundary controls before the
        // independent history oracle, including after the restarted continuation.
        let actual_increment = if step == 0 { 0.0 } else { 300400.0 * 4.0 / 1e6 };
        assert!(
            (after.observed_primary_cost - before.observed_primary_cost - actual_increment).abs()
                < 1e-12
        );
        assert_eq!(after.observed_reviewer_cost, before.observed_reviewer_cost);
        assert_eq!(after.reviewer_cost, before.reviewer_cost);
        assert_eq!(
            after.coverage.reviewer_responses,
            before.coverage.reviewer_responses
        );
        assert_eq!(
            after.coverage.primary_responses,
            before.coverage.primary_responses + step as u64 + 1
        );
        assert_eq!(after.unavailable_primary_cost_calls, u64::from(unknown));
        assert_eq!(
            after.matched_primary_responses,
            before.matched_primary_responses + step as u64 + 1 - u64::from(unknown)
        );
        assert_eq!(
            after.excluded_unpriced_calls,
            before.excluded_unpriced_calls + u64::from(unknown)
        );
        assert_eq!(after.coverage.missing_usage, before.coverage.missing_usage);
        assert_eq!(
            serde_json::to_value(&after.cache_policy_boundaries).unwrap(),
            serde_json::to_value(&before.cache_policy_boundaries).unwrap()
        );
        if step == 0 && unknown {
            assert_eq!(after.carry_cost, before.carry_cost);
            assert_eq!(after.traditional_cost, before.traditional_cost);
            assert_eq!(
                after.matched_primary_responses,
                before.matched_primary_responses
            );
            assert_eq!(
                after.excluded_unpriced_calls,
                before.excluded_unpriced_calls + 1
            );
        }
        if step == 1 {
            // Recovery prices only the newly observed dispatch. The independent
            // hypothetical cursor advanced through the unpriced interval: its
            // original modeled write covers 21,300 of this 21,400-token prompt.
            // Actual native expense still selects the long-input tariff.
            let recovered_traditional = (21300.0 * 0.1 + 100.0 * 2.5) / 1e6;
            assert!((after.carry_cost - before.carry_cost - actual_increment).abs() < 1e-12);
            assert!(
                (after.traditional_cost - before.traditional_cost - recovered_traditional).abs()
                    < 1e-12,
                "RECOVERED_NATIVE_HISTORY_PRICE: only the new known pair is admitted"
            );
        }
        let quantity = (
            t.history.retired,
            t.history.summary,
            t.history.last_traditional_input,
        );
        let expected = (280000, 1000, 21300 + step as u64 * 100);
        if quantity != expected {
            differences.push(format!(
                "step {step}: quantities {quantity:?} != {expected:?}"
            ));
        }
        if after.initial_compactions != before.initial_compactions
            || after.traditional_compactions != before.traditional_compactions
            || after.summary_cost != before.summary_cost
        {
            differences.push(format!(
                "step {step}: tariff availability repeated initialization/summary"
            ));
        }
        let cursor = state(&t)["history_continuity"]["cursor"]["input"].as_u64();
        if cursor != Some(input) {
            differences.push(format!(
                "step {step}: independent native cursor {cursor:?} != {input}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "UNKNOWN_PRICE_HISTORY_OWNER: {differences:?}"
    );
}

#[test]
fn unknown_price_preserves_free_initialized_native_growth_across_restart() {
    history_through_tariff_gap(true, true);
}

#[test]
fn unknown_price_preserves_paid_summary_native_growth_across_restart() {
    history_through_tariff_gap(false, true);
}

#[test]
fn known_zero_price_preserves_free_initialized_native_growth_across_restart() {
    history_through_tariff_gap(true, false);
}

#[test]
fn known_zero_price_preserves_paid_summary_native_growth_across_restart() {
    history_through_tariff_gap(false, false);
}
