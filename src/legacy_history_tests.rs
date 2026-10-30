// Tests-first load/continuation boundary for review5712117725. These are old
// opaque receipt histories, not genuinely new windows and not typed cursors.
// No decoder/settlement production changes accompany this tracer.
fn legacy_history_continuation(proof: &str, initialized: bool) {
    use carry::models::{Route, resolve_model};
    let current = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let mut prior = current.clone();
    prior.scenario_cache_policy.as_mut().unwrap().version = 1;
    let mut t = Trajectory::default();
    let mut request = body();
    request["max_output_tokens"] = json!(1);
    if !initialized {
        t.primary_resolved(
            &request,
            &terminal(10000, 0, 0, 0),
            "legacy-load",
            99,
            true,
            Some(&prior),
        );
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"grow into a paid summary"}));
    }
    for (offset, input) in [300000, 300100, 300200, 300300].into_iter().enumerate() {
        if offset > 0 {
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("legacy growth {offset}")}));
        }
        t.primary_resolved(
            &request,
            &terminal(input, 0, 0, 0),
            "legacy-load",
            100 + offset as u64,
            true,
            Some(if offset == 3 { &current } else { &prior }),
        );
    }
    t.reviewer(&request, &terminal(2000, 1000, 0, 7), true);
    let before = t.snapshot();
    assert_eq!(before.initial_compactions, u64::from(initialized));
    assert_eq!(before.traditional_compactions, u64::from(!initialized));
    assert_eq!(before.summary_cost > 0.0, !initialized);
    assert!(before.observed_reviewer_cost > 0.0);
    assert!(!before.cache_policy_boundaries.is_empty());
    assert_eq!(t.history.retired, 280000);
    assert_eq!(t.history.summary, 1000);
    assert_eq!(t.history.last_traditional_input, 21300);
    let boundaries = serde_json::to_value(&before.cache_policy_boundaries).unwrap();
    let mut legacy = state(&t);
    legacy.as_object_mut().unwrap().remove("history_continuity");
    legacy
        .as_object_mut()
        .unwrap()
        .remove("shared_primary_history");
    legacy["receipts"]["history_anchor"] = legacy["receipts"]["anchor"].clone();
    match proof {
        "complete" => {}
        "missing" => {
            legacy["receipts"]["configuration"] = Value::Null;
            legacy["receipts"]["history_anchor"] = Value::Null;
            legacy["receipts"]["anchor"] = Value::Null;
        }
        "conflicting" => {
            // A well-formed opaque digest, but not the frozen anchor's contract.
            legacy["receipts"]["configuration"] = json!("0".repeat(64));
        }
        _ => unreachable!(),
    }
    // Decoder normalization is allowed; quantities/money cannot change and the
    // normalized representation must be idempotent on a second load.
    let mut normalized = None;
    for _ in 0..2 {
        t = serde_json::from_value(legacy.clone()).unwrap();
        let loaded = t.snapshot();
        assert_eq!(loaded.observed_primary_cost, before.observed_primary_cost);
        assert_eq!(loaded.observed_reviewer_cost, before.observed_reviewer_cost);
        assert_eq!(loaded.carry_cost, before.carry_cost);
        assert_eq!(loaded.traditional_cost, before.traditional_cost);
        assert_eq!(loaded.summary_cost, before.summary_cost);
        assert_eq!(loaded.initial_compactions, before.initial_compactions);
        assert_eq!(
            loaded.traditional_compactions,
            before.traditional_compactions
        );
        assert_eq!(
            serde_json::to_value(&loaded.cache_policy_boundaries).unwrap(),
            boundaries
        );
        assert_eq!(t.history.retired, 280000);
        assert_eq!(t.history.summary, 1000);
        assert_eq!(t.history.last_traditional_input, 21300);
        legacy = state(&t);
        if let Some(first) = &normalized {
            assert_eq!(&legacy, first);
        }
        normalized = Some(legacy.clone());
    }
    let mut mismatches = Vec::new();
    let mut observed = before.observed_primary_cost;
    let mut comparison = before.traditional_cost;
    for step in 0..2_u64 {
        t = serde_json::from_value(state(&t)).unwrap();
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":format!("after legacy load {step}")}));
        let input = 300400 + step * 100;
        t.primary_resolved(
            &request,
            &terminal(input, 0, 0, 0),
            "legacy-load",
            104 + step,
            true,
            Some(&current),
        );
        let after = t.snapshot();
        observed += input as f64 * 4.0 / 1e6;
        // These controls precede grading unknown continuation. Actual expense
        // and existing reviewer money are known even if comparison is not.
        assert!((after.observed_primary_cost - observed).abs() < 1e-12);
        assert_eq!(after.unavailable_primary_cost_calls, 0);
        assert_eq!(
            after.coverage.primary_responses,
            before.coverage.primary_responses + step + 1
        );
        assert_eq!(after.observed_reviewer_cost, before.observed_reviewer_cost);
        assert_eq!(after.reviewer_cost, before.reviewer_cost);
        assert_eq!(
            serde_json::to_value(&after.cache_policy_boundaries).unwrap(),
            boundaries
        );
        let expected_input = if proof == "complete" {
            21400 + step * 100
        } else {
            21300
        };
        let expected_matched =
            before.matched_primary_responses + if proof == "complete" { step + 1 } else { 0 };
        let expected_excluded =
            before.excluded_unpriced_calls + if proof == "complete" { 0 } else { step + 1 };
        if proof == "complete" {
            comparison += scenario_input_cost_resolved(
                "gpt-6.1-sol",
                expected_input,
                expected_input - 100,
                Some(1024),
                Some(&current),
            )
            .unwrap();
        }
        if after.initial_compactions != before.initial_compactions
            || after.traditional_compactions != before.traditional_compactions
            || after.summary_cost != before.summary_cost
            || t.history.retired != 280000
            || t.history.summary != 1000
            || t.history.last_traditional_input != expected_input
            || after.matched_primary_responses != expected_matched
            || after.excluded_unpriced_calls != expected_excluded
            || (after.traditional_cost - comparison).abs() >= 1e-12
            || (after.carry_cost
                - if proof == "complete" {
                    before.carry_cost + observed - before.observed_primary_cost
                } else {
                    before.carry_cost
                })
            .abs()
                >= 1e-12
        {
            mismatches.push(
                json!({"step":step, "initializations":after.initial_compactions,
                "paid_summaries":after.traditional_compactions,"summary_cost":after.summary_cost,
                "retired":t.history.retired,"summary":t.history.summary,
                "input":t.history.last_traditional_input,"expected_input":expected_input,
                "matched":after.matched_primary_responses,"expected_matched":expected_matched,
                "excluded":after.excluded_unpriced_calls,"expected_excluded":expected_excluded,
                "traditional_cost":after.traditional_cost,"expected_cost":comparison}),
            );
        }
    }
    assert!(
        mismatches.is_empty(),
        "LEGACY_HISTORY_PROOF: missing/conflicting proof must freeze legacy history and comparisons, never adopt/reinitialize it; proof={proof}, initialized={initialized}, mismatches={mismatches:?}"
    );
}

#[test]
fn legacy_exact_contract_retains_growth_and_frozen_money_across_reloads() {
    for initialized in [true, false] {
        legacy_history_continuation("complete", initialized);
    }
}

#[test]
fn legacy_missing_contract_keeps_free_history_read_only_across_reloads() {
    legacy_history_continuation("missing", true);
}

#[test]
fn legacy_missing_contract_keeps_paid_history_read_only_across_reloads() {
    legacy_history_continuation("missing", false);
}

#[test]
fn legacy_conflicting_contract_keeps_free_history_read_only_across_reloads() {
    legacy_history_continuation("conflicting", true);
}

#[test]
fn legacy_conflicting_contract_keeps_paid_history_read_only_across_reloads() {
    legacy_history_continuation("conflicting", false);
}
