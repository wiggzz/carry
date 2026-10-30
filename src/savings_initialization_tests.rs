// Issue186: native trace/report estimator, separate from proxy native receipts.
// These tests intentionally precede production changes.
#[test]
fn initialization186_native_first_primary_is_reduced_and_later_summary_is_paid() {
    let mut t = Trajectory::with_scenario(Scenario {
        context_window: 3000,
        reserve: 100,
        keep_recent: 1200,
        summary: 20,
    });
    let mut body = json!({"model":"gpt-6.1-sol", "input":[
        {"role":"assistant", "content":" evidence".repeat(4000)},
        {"role":"user", "content":"current"}
    ]});
    let first_input = prompt_projection(&body).unwrap().2;
    assert!(
        first_input > 2900,
        "fixture really exceeds the economic budget"
    );
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let first = response(&mut t, first_input, 0, 0);
    assert!(first.priced);
    assert!(first.observed_priced);
    let expected_paid = estimated_cost_usd(
        "gpt-6.1-sol",
        &Usage {
            input_tokens: first_input,
            ..Usage::default()
        },
    )
    .unwrap();
    assert!((first.observed_cost - expected_paid).abs() < 1e-12);
    assert_eq!(first.summary_cost, 0.0, "pre-window initialization is free");
    assert_eq!(
        first.pi_compactions, 0,
        "this counter counts charged in-window work"
    );
    let cold = input_cost("gpt-6.1-sol", 1220, 0, Some(1024)).unwrap();
    assert!(
        (first.pi_cost - cold).abs() < 1e-12,
        "first primary must use initialized prompt"
    );
    body["input"].as_array_mut().unwrap().push(json!({
        "role":"assistant", "content":" later".repeat(4000)
    }));
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let second = response(&mut t, prompt_projection(&body).unwrap().2, 0, 0);
    assert!(second.priced);
    assert_eq!(second.pi_compactions, 1);
    assert!(
        second.summary_cost > 0.0,
        "later in-window compaction remains paid"
    );
}

#[test]
fn initialization186_native_fixed_overhead_infeasible_preserves_observed_money() {
    let mut t = Trajectory::with_scenario(Scenario {
        context_window: 3000,
        reserve: 100,
        keep_recent: 1200,
        summary: 20,
    });
    let body = json!({"model":"gpt-6.1-sol", "instructions":" fixed".repeat(3000),
        "input":[{"role":"user","content":"current protected goal"}]});
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let estimate = response(&mut t, 3100, 0, 0);
    assert!(estimate.observed_priced);
    assert!(estimate.observed_cost > 0.0);
    assert!(
        !estimate.priced,
        "cannot compact fixed instructions to invent a feasible comparison"
    );
}
