// Independently reviewed R2: delayed quantity evidence must settle transitions,
// never the excluded historical money.
#[test]
fn unknown_retention_resolves_summary_before_next_primary_after_restart() {
    let base = body();
    let opaque = json!({"type":"reasoning","summary":[],"encrypted_content":"retained-unknown"});
    let mut next = base.clone();
    next["input"].as_array_mut().unwrap().push(opaque.clone());
    let mut m = crate::proxy_quantity::InputMeasurements::default();
    m.observe(&base, &json!({"input_tokens":1000}), "route", None, 1);
    m.observe(
        &next,
        &json!({"input_tokens":6000 + crate::savings::local_text_tokens(&opaque)}),
        "route",
        None,
        2,
    );
    let scenario = Scenario {
        context_window: 4000,
        reserve: 100,
        keep_recent: 1500,
        summary: 100,
    };
    let mut unknown = Trajectory {
        scenario,
        ..Trajectory::default()
    };
    let mut known = unknown.clone();
    let mut response = terminal(1000, 0, 0, 0);
    response["output"] = json!([opaque]);
    for (t, measurements) in [(&mut unknown, None), (&mut known, Some(&m))] {
        t.primary_settled(PrimaryObservation {
            body: &base,
            response: &response,
            route: "route",
            at: 10,
            standard: true,
            resolved: None,
            cost: Some(0.002),
            measurements,
        });
    }
    assert_eq!(known.history.retired, 4500);
    let frozen = unknown.snapshot();
    unknown = serde_json::from_value(state(&unknown)).unwrap();
    known = serde_json::from_value(state(&known)).unwrap();
    for t in [&mut unknown, &mut known] {
        t.primary(&next, &terminal(6000, 0, 0, 0), "route", 11, true);
    }
    assert_eq!(
        unknown.history.last_traditional_input, 1600,
        "the unresolved response crossed the threshold BEFORE this primary"
    );
    assert_eq!(unknown.history.retired, known.history.retired);
    assert_eq!(unknown.history.summary, known.history.summary);
    assert_eq!(
        unknown.snapshot().traditional_compactions,
        known.snapshot().traditional_compactions
    );
    assert_eq!(unknown.snapshot().initial_compactions, 0);
    assert_eq!(
        unknown.snapshot().summary_cost,
        frozen.summary_cost,
        "do not backfill excluded summary money"
    );
    assert_eq!(unknown.snapshot().matched_primary_responses, 1);
    assert!(
        (unknown.snapshot().traditional_cost
            - scenario_input_cost("gpt-6.1-sol", 1600, 0, Some(1024)).unwrap())
        .abs()
            < 1e-12
    );
    assert_eq!(
        unknown.snapshot().observed_primary_cost,
        known.snapshot().observed_primary_cost
    );
}

#[test]
fn unknown_retention_advances_known_divergent_cache_before_restart() {
    use carry::models::{Route, resolve_model};
    let resolved = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let mut request = body();
    request["prompt_cache_options"] = json!({"mode":"implicit","ttl":"30m"});
    let opaque = json!({"type":"reasoning","summary":[],"encrypted_content":"cache-gap"});
    let mut next = request.clone();
    next["input"].as_array_mut().unwrap().push(opaque.clone());
    let mut m = crate::proxy_quantity::InputMeasurements::default();
    m.observe(
        &request,
        &json!({"input_tokens":2000}),
        "route",
        Some(&resolved),
        1,
    );
    m.observe(
        &next,
        &json!({"input_tokens":2100 + crate::savings::local_text_tokens(&opaque)}),
        "route",
        Some(&resolved),
        2,
    );
    let mut unknown = Trajectory::default();
    unknown.projected(100, 100, true);
    unknown.primary_resolved(
        &request,
        &terminal(2000, 0, 0, 0),
        "route",
        100,
        true,
        Some(&resolved),
    );
    let mut known = unknown.clone();
    request["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"new input"}));
    let mut response = terminal(2100, 2000, 0, 0);
    response["output"] = json!([opaque.clone()]);
    for (t, measurements) in [(&mut unknown, None), (&mut known, Some(&m))] {
        t.primary_settled(PrimaryObservation {
            body: &request,
            response: &response,
            route: "route",
            at: 1800,
            standard: true,
            resolved: Some(&resolved),
            cost: Some(0.001),
            measurements,
        });
    }
    assert_eq!(
        state(&unknown)["receipts"]["modeled_cache"],
        state(&known)["receipts"]["modeled_cache"],
        "known input writes and read refresh survive an unknown retained output"
    );
    unknown = serde_json::from_value(state(&unknown)).unwrap();
    known = serde_json::from_value(state(&known)).unwrap();
    request["input"].as_array_mut().unwrap().push(opaque);
    let before_unknown = unknown.snapshot();
    let before_known = known.snapshot();
    for t in [&mut unknown, &mut known] {
        t.primary_resolved(
            &request,
            &terminal(2200, 0, 0, 0),
            "route",
            2000,
            true,
            Some(&resolved),
        );
    }
    assert!(
        (unknown.snapshot().traditional_cost
            - before_unknown.traditional_cost
            - (known.snapshot().traditional_cost - before_known.traditional_cost))
            .abs()
            < 1e-12
    );
    assert_eq!(
        unknown.snapshot().observed_primary_cost,
        known.snapshot().observed_primary_cost
    );
}

#[test]
fn successful_unknown_projection_cannot_reopen_free_initialization() {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 4000,
            reserve: 100,
            keep_recent: 1500,
            summary: 100,
        },
        ..Trajectory::default()
    };
    let mut full = body();
    full["input"].as_array_mut().unwrap().insert(
        0,
        json!({"type":"reasoning","summary":[],"encrypted_content":"initial-no-attribution"}),
    );
    t.primary(&full, &terminal(1000, 0, 0, 0), "route", 1, true);
    full["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"growth"}));
    t.primary(&full, &terminal(5000, 0, 0, 0), "route", 2, true);
    let before = t.snapshot();
    assert_eq!(before.traditional_compactions, 1);
    assert_eq!(t.history.retired, 3500);
    let mut partial = full.clone();
    partial["input"].as_array_mut().unwrap().remove(0);
    let mut m = crate::proxy_quantity::InputMeasurements::default();
    m.observe(&full, &json!({"input_tokens":5000}), "route", None, 2);
    let unavailable = m.accounting_restoration(&full, &partial, "route", None);
    assert!(unavailable.is_none());
    t.projected_evidence(unavailable, true);
    t.primary(&partial, &terminal(3000, 0, 0, 0), "route", 3, true);
    assert_eq!(
        t.history.retired, 3500,
        "quantity-unavailable success is not a canonical rebase"
    );
    assert!(t.history.history_continuity.is_some());
    assert_eq!(t.snapshot().traditional_cost, before.traditional_cost);
    assert_eq!(t.snapshot().summary_cost, before.summary_cost);
    t = serde_json::from_value(state(&t)).unwrap();
    // Compact -> off: zero restoration is now measurable, but it must never
    // turn this existing window into another pre-window/free summary.
    t.projected_input(Some(0), true);
    t.primary(&full, &terminal(5000, 0, 0, 0), "route", 4, true);
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert!(t.history.retired >= 3500);
    full["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"later growth"}));
    t.projected_input(Some(0), false);
    t.primary(&full, &terminal(9000, 0, 0, 0), "route", 5, true);
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert!(t.snapshot().traditional_cost >= before.traditional_cost);
    assert!(t.snapshot().summary_cost >= before.summary_cost);
    assert_eq!(t.snapshot().coverage.primary_responses, 5);
    assert!(t.snapshot().observed_primary_cost > before.observed_primary_cost);
    assert!(t.snapshot().matched_primary_responses >= 2);
    if t.snapshot().matched_primary_responses == 2 {
        assert_eq!(
            t.snapshot().coverage.unavailable_quantity_calls,
            3,
            "unresolved continuation must remain explicitly unavailable"
        );
    } else {
        assert!(
            t.snapshot().traditional_compactions >= 2,
            "recovered growth must pay its next summary"
        );
    }
}

#[test]
fn unavailable_intervening_retention_cannot_later_skip_successful_transitions() {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 4000,
            reserve: 100,
            keep_recent: 1500,
            summary: 100,
        },
        ..Trajectory::default()
    };
    let base = body();
    let opaque =
        json!({"type":"reasoning","summary":[],"encrypted_content":"unresolved-intermediate"});
    let mut response = terminal(1000, 0, 0, 0);
    response["output"] = json!([opaque.clone()]);
    t.primary(&base, &response, "route", 1, true);
    let mut next = base.clone();
    next["input"].as_array_mut().unwrap().extend([
        opaque,
        json!({"role":"user","content":"unattributed large append"}),
    ]);
    t.primary(&next, &terminal(6000, 0, 0, 0), "route", 2, true);
    let frozen = t.snapshot();
    assert_eq!(frozen.matched_primary_responses, 0);
    t = serde_json::from_value(state(&t)).unwrap();
    let mut m = crate::proxy_quantity::InputMeasurements::default();
    m.observe(&base, &json!({"input_tokens":1000}), "route", None, 1);
    m.observe(&next, &json!({"input_tokens":6000}), "route", None, 2);
    t.primary_settled(PrimaryObservation {
        body: &next,
        response: &terminal(6000, 0, 0, 0),
        route: "route",
        at: 3,
        standard: true,
        resolved: None,
        cost: Some(0.1),
        measurements: Some(&m),
    });
    assert_eq!(
        t.snapshot().matched_primary_responses,
        0,
        "late calibration cannot silently skip a successful intervening request's state"
    );
    assert_eq!(t.snapshot().traditional_cost, frozen.traditional_cost);
    assert_eq!(t.snapshot().coverage.unavailable_quantity_calls, 3);
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert!(t.snapshot().observed_primary_cost > frozen.observed_primary_cost);
}

#[test]
fn mode_off_missing_resolution_metadata_is_not_a_new_traditional_window() {
    use carry::models::{Route, resolve_model};
    let resolved = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 4000,
            reserve: 100,
            keep_recent: 1500,
            summary: 100,
        },
        ..Trajectory::default()
    };
    let mut request = body();
    request["max_output_tokens"] = json!(1);
    t.primary_resolved(
        &request,
        &terminal(1000, 0, 0, 0),
        "route",
        1,
        true,
        Some(&resolved),
    );
    request["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"paid growth"}));
    t.primary_resolved(
        &request,
        &terminal(5000, 0, 0, 0),
        "route",
        2,
        true,
        Some(&resolved),
    );
    let frozen = t.snapshot();
    t.primary(&request, &terminal(5000, 0, 0, 0), "route", 3, true);
    t = serde_json::from_value(state(&t)).unwrap();
    t.primary_resolved(
        &request,
        &terminal(5000, 0, 0, 0),
        "route",
        4,
        true,
        Some(&resolved),
    );
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert_eq!(t.history.retired, 3500);
    assert_eq!(t.history.last_traditional_input, 1600);
    assert_eq!(t.snapshot().summary_cost, frozen.summary_cost);
    assert_eq!(t.snapshot().traditional_compactions, 1);
}

#[test]
fn resolved_zero_projection_gap_recovers_transitions_not_historical_money() {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 4000,
            reserve: 100,
            keep_recent: 1500,
            summary: 100,
        },
        ..Trajectory::default()
    };
    let mut request = body();
    t.primary(&request, &terminal(1000, 0, 0, 0), "route", 1, true);
    request["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"paid summary growth"}));
    t.primary(&request, &terminal(5000, 0, 0, 0), "route", 2, true);
    let frozen = t.snapshot();
    t.projected_input(None, true);
    t.primary(&request, &terminal(5000, 0, 0, 0), "route", 3, true);
    t = serde_json::from_value(state(&t)).unwrap();
    assert!(t.history.unresolved_projection);
    assert_eq!(t.snapshot().traditional_cost, frozen.traditional_cost);
    t.projected_input(Some(0), false);
    t.primary(&request, &terminal(5000, 0, 0, 0), "route", 4, true);
    assert!(
        !t.history.unresolved_projection,
        "exact zero-restoration repeat resolves a single empty-output gap"
    );
    assert_eq!(t.snapshot().matched_primary_responses, 3);
    assert_eq!(t.snapshot().coverage.unavailable_quantity_calls, 1);
    assert_eq!(t.snapshot().summary_cost, frozen.summary_cost);
    assert_eq!(t.history.retired, 3500);
    assert_eq!(t.snapshot().initial_compactions, 0);
    let increment = t.snapshot().traditional_cost - frozen.traditional_cost;
    assert!(
        (increment - scenario_input_cost("gpt-6.1-sol", 1600, 1600, Some(1024)).unwrap()).abs()
            < 1e-12,
        "recover the excluded successful input's cache write at its original clock"
    );
    request["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"further growth"}));
    t.primary(&request, &terminal(8000, 0, 0, 0), "route", 5, true);
    assert_eq!(t.snapshot().traditional_compactions, 2);
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert!(t.snapshot().summary_cost > frozen.summary_cost);
}

#[test]
fn dashboard_aggregates_partial_scope_quality_and_legacy_provenance() {
    let mut partial = Trajectory::default();
    partial.primary(&body(), &terminal(1000, 0, 0, 0), "route", 1, true);
    partial.projected_input(None, true);
    partial.primary(&body(), &terminal(1100, 0, 0, 0), "route", 2, true);
    let mut complete = Trajectory::default();
    complete.primary(&body(), &terminal(1000, 0, 0, 0), "route", 1, true);
    let mut legacy = complete.snapshot();
    legacy.quantity_policy_version = None;
    legacy.coverage.measured_quantity_calls = 0;
    let p = partial.snapshot().dashboard(false);
    let c = complete.snapshot().dashboard(false);
    let l = legacy.dashboard(false);
    let mut totals = DashboardTotals::default();
    totals.observe(&c);
    let serialized = serde_json::to_value(&totals).unwrap();
    assert_eq!(
        serialized["quantity_evidence"]["comparison_scope"],
        "matched_completed_primary_window"
    );
    totals.observe(&p);
    totals.observe(&l);
    let serialized = serde_json::to_value(&totals).unwrap();
    assert_eq!(
        serialized["quantity_evidence"]["comparison_scope"],
        "matched_subset_not_whole_session"
    );
    assert_eq!(
        serialized["quantity_evidence"]["total_primary_responses"],
        4
    );
    assert_eq!(serialized["matched_primary_responses"], 3);
    assert_eq!(serialized["quantity_evidence"]["measured_priced_calls"], 2);
    assert_eq!(
        serialized["quantity_evidence"]["unavailable_quantity_calls"],
        1
    );
    assert_eq!(serialized["quantity_evidence"]["legacy_priced_calls"], 1);
    assert_eq!(
        serialized["quantity_evidence"]["historical_policy_scope"],
        "mixed_prospective_only_no_repricing"
    );
    let carry = serialized["carry_cost"].as_f64().unwrap();
    let traditional = serialized["traditional_cost"].as_f64().unwrap();
    assert_eq!(
        serialized["savings_percent"].as_f64().unwrap(),
        100.0 * (traditional - carry) / traditional
    );
}

// Existing continuous trajectory, driven by the SAME input-evidence owner used
// by dispatch. Synthetic native units deliberately differ from local units.
#[test]
fn calibrated_partial_projection_replaces_omission_through_summary_restart_and_failure() {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 4000,
            reserve: 100,
            keep_recent: 1500,
            summary: 100,
        },
        ..Trajectory::default()
    };
    let mut measurements = crate::proxy_quantity::InputMeasurements::default();
    let base = body();
    measurements.observe(&base, &json!({"input_tokens":1000}), "route", None, 1);
    t.primary(&base, &terminal(1000, 0, 0, 0), "route", 1, true);
    let mut full = base.clone();
    full["input"].as_array_mut().unwrap().extend([
        json!({"type":"reasoning","id":"first","summary":[],"encrypted_content":"first-opaque"}),
        json!({"type":"reasoning","id":"second","summary":[],"encrypted_content":"second-opaque"}),
        json!({"role":"user","content":"added visible"}),
    ]);
    measurements.observe(&full, &json!({"input_tokens":5000}), "route", None, 2);
    t.primary(&full, &terminal(5000, 0, 0, 0), "route", 2, true);
    assert_eq!(t.snapshot().traditional_compactions, 1);
    let summary_cost = t.snapshot().summary_cost;
    let mut sent = full.clone();
    sent["input"].as_array_mut().unwrap().remove(1);
    let restored = measurements
        .accounting_restoration(&full, &sent, "route", None)
        .unwrap();
    assert!(restored.estimated_opaque_items > 0);
    assert!(restored.tokens > 0);
    for at in [3, 4] {
        t.projected_evidence(Some(restored), at == 3);
        t.primary(&sent, &terminal(3000, 0, 0, 0), "route", at, true);
        assert_eq!(
            t.history.last_traditional_input,
            (3000 + restored.tokens - 3500 + 100) as u64
        );
        assert_eq!(
            t.snapshot().summary_cost,
            summary_cost,
            "repeat omission cannot accumulate or charge another summary"
        );
        t = serde_json::from_value(state(&t)).unwrap();
    }
    t.failed_call();
    t = serde_json::from_value(state(&t)).unwrap();
    sent["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"append after failure"}));
    let mut appended_full = full.clone();
    appended_full["input"]
        .as_array_mut()
        .unwrap()
        .push(sent["input"].as_array().unwrap().last().unwrap().clone());
    t.projected_evidence(
        measurements.accounting_restoration(&appended_full, &sent, "route", None),
        true,
    );
    t.primary(&sent, &terminal(3100, 0, 0, 0), "route", 5, true);
    assert_eq!(
        t.history.last_traditional_input,
        (3100 + restored.tokens - 3500 + 100) as u64
    );
    assert_eq!(t.snapshot().matched_primary_responses, 5);
    assert_eq!(t.snapshot().traditional_compactions, 1);
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert_eq!(t.snapshot().coverage.estimated_quantity_calls, 4);
    let dashboard = serde_json::to_value(t.snapshot().dashboard(false)).unwrap();
    assert_eq!(
        dashboard["quantity_evidence"]["comparison_scope"],
        "matched_subset_not_whole_session"
    );
    // A larger replacement memory is a NEGATIVE current restoration, not zero
    // and not the previous omission plus a new edit.
    let mut expanded = full.clone();
    expanded["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"expanded memory".repeat(100)}));
    let expansion = measurements
        .accounting_restoration(&full, &expanded, "route", None)
        .unwrap();
    assert!(expansion.tokens < 0);
    t.projected_evidence(Some(expansion), true);
    t.primary(&expanded, &terminal(6000, 0, 0, 0), "route", 6, true);
    assert_eq!(
        t.history.last_traditional_input,
        (6000 + expansion.tokens - 3500 + 100) as u64
    );
    t = serde_json::from_value(state(&t)).unwrap();
    expanded["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"later growth crosses another summary"}));
    t.projected_evidence(Some(expansion), true);
    t.primary(&expanded, &terminal(12000, 0, 0, 0), "route", 7, true);
    assert_eq!(t.snapshot().traditional_compactions, 2);
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert!(t.snapshot().summary_cost > summary_cost);
    assert_eq!(t.history.last_traditional_input, 1600);
}

#[test]
fn zero_residual_hidden_retention_is_still_estimated_not_measured() {
    let mut measurements = crate::proxy_quantity::InputMeasurements::default();
    let base = body();
    measurements.observe(&base, &json!({"input_tokens":1000}), "route", None, 1);
    let opaque = json!({"type":"reasoning","summary":[],"encrypted_content":"zero-residual"});
    let input = 1000 + crate::savings::local_text_tokens(&opaque);
    let mut appended = base.clone();
    appended["input"]
        .as_array_mut()
        .unwrap()
        .push(opaque.clone());
    measurements.observe(&appended, &json!({"input_tokens":input}), "route", None, 2);
    let mut response = terminal(input, 0, 0, 0);
    response["output"] = json!([opaque]);
    let mut t = Trajectory::default();
    t.primary_settled(PrimaryObservation {
        body: &appended,
        response: &response,
        route: "route",
        at: 3,
        standard: true,
        resolved: None,
        cost: Some(0.01),
        measurements: Some(&measurements),
    });
    assert_eq!(t.snapshot().matched_primary_responses, 1);
    assert_eq!(t.snapshot().coverage.estimated_quantity_calls, 1);
    assert_eq!(t.snapshot().coverage.measured_quantity_calls, 0);
    assert_eq!(t.snapshot().carry_cost, t.snapshot().traditional_cost);
}

#[test]
fn quantity_policy_upgrade_freezes_prior_money_and_exclusions_exactly_once() {
    let mut t = Trajectory::default();
    t.primary(&body(), &terminal(1000, 0, 10, 0), "route", 1, true);
    t.reviewer_usage_settled(
        &json!({"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":0}}),
        true,
        Some(0.25),
    );
    t.projected_input(None, true);
    t.primary(&body(), &terminal(1100, 0, 10, 0), "route", 2, true);
    let prior = t.snapshot();
    let mut saved = state(&t);
    saved["receipts"]
        .as_object_mut()
        .unwrap()
        .remove("quantity_policy_version");
    t = serde_json::from_value(saved).unwrap();
    for at in [3, 4] {
        t.projected_input(Some(0), false);
        t.primary(&body(), &terminal(1100, 0, 10, 0), "route", at, true);
        t = serde_json::from_value(state(&t)).unwrap();
        let after = t.snapshot();
        let frozen = after.legacy_quantity_segment.as_ref().unwrap();
        assert_eq!(frozen.carry_cost, prior.carry_cost);
        assert_eq!(frozen.traditional_cost, prior.traditional_cost);
        assert_eq!(frozen.coverage.primary_responses, 2);
        assert_eq!(frozen.matched_primary_responses, 1);
        assert_eq!(after.reviewer_cost, 0.25);
        assert_eq!(after.coverage.unavailable_quantity_calls, 1);
        assert_eq!(after.quantity_policy_version, Some(1));
    }
}
