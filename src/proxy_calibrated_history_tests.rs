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
