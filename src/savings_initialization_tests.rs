// Issue186: native trace/report estimator, separate from proxy native receipts.
// These tests intentionally precede production changes.
#[test]
fn initialization186_native_paid_compaction_retains_fixed_floor() {
    let scenario = Scenario {
        context_window: 3000,
        reserve: 100,
        keep_recent: 1200,
        summary: 20,
    };
    // Fixed tools/instructions fit the economic budget but exceed keep_recent.
    let mut body = json!({"model":"gpt-6.1-sol", "instructions":" fixed".repeat(1600),
        "tools":[{"type":"function", "name":"owned", "description":"fixed tool",
                  "parameters":{"type":"object", "properties":{}}}],
        "input":[{"role":"assistant", "content":" evidence".repeat(4000)},
                 {"role":"user", "content":"current"}]});
    let protected = protected_prompt_tokens(&body).unwrap();
    assert!(protected > scenario.keep_recent);
    assert!(protected + scenario.summary < scenario.context_window - scenario.reserve);
    let mut t = Trajectory::with_scenario(scenario);
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let first = response(&mut t, prompt_projection(&body).unwrap().2, 0, 0);
    assert!(first.priced && first.observed_priced);
    assert_eq!(first.initial_compactions, 1);
    assert_eq!(first.pi_compactions, 0);
    assert_eq!(first.summary_cost, 0.0);
    body["input"].as_array_mut().unwrap().push(json!({
        "role":"assistant", "content":" later".repeat(4000)
    }));
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let second = response(&mut t, prompt_projection(&body).unwrap().2, 0, 0);
    assert!(second.priced && second.observed_priced);
    assert_eq!(second.initial_compactions, 1);
    assert_eq!(second.pi_compactions, 1);
    assert!(second.summary_cost > 0.0);
    let fixed = token_units(&json!({"instructions":body.get("instructions"),
        "tools":body.get("tools"),"text":body.get("text")}));
    assert!(
        t.last_pi_input >= fixed + scenario.summary,
        "later paid compaction cannot retire fixed instructions/tools below their floor"
    );
    body["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user", "content":"continue"}));
    let input = prompt_projection(&body).unwrap().2;
    let modeled = (input as i128 - t.removed as i128 + t.summary as i128) as u64;
    assert!(modeled >= fixed + scenario.summary);
    let cold = input_cost("gpt-6.1-sol", modeled, 0, Some(1024)).unwrap();
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let third = response(&mut t, input, 0, 0);
    assert!(third.priced && third.observed_priced);
    assert_eq!(third.pi_compactions, 1);
    assert_eq!(third.summary_cost, second.summary_cost);
    assert!((third.pi_cost - second.pi_cost - cold).abs() < 1e-12);
    assert!(third.observed_cost > second.observed_cost);
}
#[test]
fn initialization186_native_reserves_requested_output_before_first_primary() {
    let scenario = Scenario {
        context_window: 3000,
        reserve: 100,
        keep_recent: 1200,
        summary: 20,
    };
    let mut body = json!({"model":"gpt-6.1-sol", "max_output_tokens":1, "input":[
        {"role":"assistant", "content":" evidence".repeat(2400)},
        {"role":"user", "content":" current".repeat(1200)}
    ]});
    let input = prompt_projection(&body).unwrap().2;
    let protected = protected_prompt_tokens(&body).unwrap();
    assert!(input > scenario.context_window - scenario.reserve);
    assert!(protected + scenario.summary < scenario.context_window - scenario.reserve);
    assert!(protected + scenario.summary + 2000 > scenario.context_window);

    let mut small_output = Trajectory::with_scenario(scenario);
    small_output.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let fitting = response(&mut small_output, input, 0, 0);
    assert!(
        fitting.priced,
        "small output reservation leaves a feasible initialized prompt"
    );
    assert_eq!(fitting.initial_compactions, 1);
    assert_eq!(fitting.summary_cost, 0.0);

    body["max_output_tokens"] = json!(2000);
    let mut large_output = Trajectory::with_scenario(scenario);
    large_output.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let infeasible = response(&mut large_output, input, 0, 0);
    assert!(infeasible.observed_priced);
    assert_eq!(infeasible.observed_cost, fitting.observed_cost);
    assert!(
        !infeasible.priced,
        "the requested output allowance and protected current input cannot both fit"
    );
}
#[test]
fn initialization186_native_output_allowance_triggers_fitting_initialization() {
    let scenario = Scenario {
        context_window: 3000,
        reserve: 100,
        keep_recent: 1200,
        summary: 20,
    };
    let body = json!({"model":"gpt-6.1-sol", "max_output_tokens":1500, "input":[
        {"role":"assistant", "content":" evidence".repeat(2000)},
        {"role":"user", "content":"current"}
    ]});
    let input = prompt_projection(&body).unwrap().2;
    assert!(input < scenario.context_window - scenario.reserve);
    assert!(input + 1500 > scenario.context_window);
    assert!(scenario.keep_recent + scenario.summary + 1500 <= scenario.context_window);
    let mut t = Trajectory::with_scenario(scenario);
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let first = response(&mut t, input, 0, 0);
    assert!(first.priced);
    assert!(first.observed_priced);
    assert_eq!(first.initial_compactions, 1);
    assert_eq!(first.pi_compactions, 0);
    assert_eq!(first.summary_cost, 0.0);
    let cold = input_cost("gpt-6.1-sol", 1220, 0, Some(1024)).unwrap();
    assert!((first.pi_cost - cold).abs() < 1e-12);
}

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

#[test]
fn initialization186_native_fixed_tools_infeasible_preserves_observed_money() {
    let scenario = Scenario {
        context_window: 3000,
        reserve: 100,
        keep_recent: 1200,
        summary: 20,
    };
    let body = json!({"model":"gpt-6.1-sol", "tools":[{
        "type":"function", "name":"owned", "description":" fixed".repeat(3000),
        "parameters":{"type":"object", "properties":{}}
    }], "input":[{"role":"user","content":"current protected goal"}]});
    assert!(protected_prompt_tokens(&body).unwrap() > scenario.context_window - scenario.reserve);
    let mut t = Trajectory::with_scenario(scenario);
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let estimate = response(&mut t, 3100, 0, 0);
    assert!(estimate.observed_priced);
    assert!(estimate.observed_cost > 0.0);
    assert!(
        !estimate.priced,
        "fixed tools cannot be hypothetically retired"
    );
    assert_eq!(estimate.initial_compactions, 0);
    assert_eq!(estimate.pi_compactions, 0);
    assert_eq!(estimate.summary_cost, 0.0);
}

#[test]
fn initialization186_native_current_user_infeasible_preserves_observed_money() {
    let mut t = Trajectory::with_scenario(Scenario {
        context_window: 3000,
        reserve: 100,
        keep_recent: 1200,
        summary: 20,
    });
    let body = json!({"model":"gpt-6.1-sol", "input":[
        {"role":"assistant", "content":" evidence".repeat(2000)},
        {"role":"user", "content":" current".repeat(3000)}
    ]});
    t.record(&json!({"event":"model_request", "data":{"step":1,"request":body}}));
    let estimate = response(&mut t, 5100, 0, 0);
    assert!(estimate.observed_priced);
    assert!(estimate.observed_cost > 0.0);
    assert!(
        !estimate.priced,
        "initialization cannot retire the current user request to invent feasibility"
    );
}
