//! Synthetic receipts: the wire body deliberately does not predict provider units.
use super::*;

fn body() -> Value {
    json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"fixed source"}]})
}
fn terminal(input: u64, cached: u64, written: u64, output: u64) -> Value {
    json!({"status":"completed","output":[],"usage":{"input_tokens":input,"output_tokens":output,
        "input_tokens_details":{"cached_tokens":cached,"cache_write_tokens":written}}})
}
fn state(t: &Trajectory) -> Value {
    serde_json::to_value(t).unwrap()
}

#[test]
fn receipt_opaque_output_sizes_do_not_retire_native_input_after_replay_and_restart() {
    fn episode(ciphertext: &str, metadata: &str) -> Trajectory {
        let mut t = Trajectory {
            scenario: Scenario {
                context_window: 3000,
                reserve: 100,
                keep_recent: 100,
                summary: 20,
            },
            ..Trajectory::default()
        };
        let output = json!([
            {"type":"reasoning","id":metadata,"summary":[],"encrypted_content":ciphertext},
            {"type":"message","id":metadata,"role":"assistant","content":[{"type":"output_text","text":"OK"}]}
        ]);
        let mut response = terminal(2000, 0, 0, 10000);
        response["output"] = output.clone();
        t.primary(&body(), &response, "route", 100, true);
        let mut t: Trajectory = serde_json::from_value(state(&t)).unwrap();
        let mut next = body();
        next["input"]
            .as_array_mut()
            .unwrap()
            .extend(output.as_array().unwrap().clone());
        next["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"continue"}));
        t.primary(&next, &terminal(2100, 0, 0, 0), "route", 101, true);
        t
    }
    let control = episode("short", "native-id");
    for (ciphertext, metadata) in [
        ("ciphertext ".repeat(5000), "native-id".into()),
        ("short".into(), "metadata ".repeat(5000)),
    ] {
        let t = episode(&ciphertext, &metadata);
        assert_eq!(state(&t)["receipts"]["last_appended_input"], 100);
        assert_eq!(
            state(&t)["retired"],
            0,
            "opaque transport length must not become native retirement"
        );
        assert_eq!(state(&t)["last_traditional_input"], 2100);
        assert_eq!(t.snapshot().traditional_compactions, 0);
        assert_eq!(
            t.snapshot().observed_primary_cost,
            control.snapshot().observed_primary_cost
        );
        assert_eq!(t.snapshot().carry_cost, control.snapshot().carry_cost);
        assert_eq!(
            t.snapshot().traditional_cost,
            control.snapshot().traditional_cost
        );
    }
}

#[test]
fn receipt_visible_output_semantics_still_trigger_conventional_summary() {
    let payload = "visible output ".repeat(5000);
    for item in [
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":payload}]}),
        json!({"type":"reasoning","summary":[{"type":"summary_text","text":payload}]}),
        json!({"type":"function_call","name":"tool","call_id":"a","arguments":payload}),
        json!({"type":"custom_tool_call","name":"tool","call_id":"b","input":payload}),
    ] {
        let mut t = Trajectory {
            scenario: Scenario {
                context_window: 3000,
                reserve: 100,
                keep_recent: 100,
                summary: 20,
            },
            ..Trajectory::default()
        };
        let mut response = terminal(2000, 0, 0, 10000);
        response["output"] = json!([item]);
        t.primary(&body(), &response, "route", 100, true);
        assert_eq!(t.snapshot().traditional_compactions, 1);
        assert!(t.snapshot().summary_cost > 0.0);
        assert!(t.snapshot().observed_primary_cost > 0.0);
    }
}

#[test]
fn receipt_unknown_retained_output_does_not_invent_a_context_measurement() {
    let mut t = Trajectory::default();
    let mut response = terminal(2000, 0, 0, 10000);
    response["output"] = json!([{"type":"unmodeled_native_output","payload":"opaque"}]);
    t.primary(&body(), &response, "route", 100, true);
    assert!(
        t.snapshot().observed_primary_cost > 0.0,
        "paid receipt remains priced independently of retention"
    );
    assert_eq!(t.snapshot().matched_primary_responses, 0);
    assert_eq!(t.snapshot().excluded_unpriced_calls, 1);
    assert_eq!(state(&t)["retired"], 0);
}

#[test]
fn receipt_restoration_selects_each_branch_tariff_without_repricing_actuals() {
    let mut t = Trajectory::default();
    t.scenario.context_window = 1_000_000;
    t.primary(
        &body(),
        &terminal(200_000, 1000, 100, 10),
        "route",
        100,
        true,
    );
    let before = t.snapshot();
    t.compacted(100_100, 100);
    t.primary(
        &body(),
        &terminal(200_000, 1000, 100, 10),
        "route",
        101,
        true,
    );
    let after = t.snapshot();
    let expected = scenario_input_cost("gpt-6.1-sol", 300_000, 0, Some(1024)).unwrap()
        + scenario_output_cost_resolved("gpt-6.1-sol", 300_000, 10, None).unwrap();
    assert!(
        (after.traditional_cost - before.traditional_cost - expected).abs() < 1e-12,
        "estimated restoration crosses the traditional tariff threshold only"
    );
    assert!((after.carry_cost - 2.0 * before.carry_cost).abs() < 1e-12);
    assert_eq!(after.observed_primary_cost, after.carry_cost);
    assert!(state(&t)["receipts"]["last_appended_input"].is_null());
    // A configuration change after a Carry edit cannot retain its restoration.
    t.compacted(10_000, 0);
    let mut changed = body();
    changed["instructions"] = json!("a new configuration");
    t.primary(&changed, &terminal(200_000, 0, 0, 0), "route", 102, true);
    assert_eq!(state(&t)["restored"], 0);
}

#[test]
fn receipt_counterfactual_cache_is_conditional_not_copied_from_actual_hits() {
    for (input, delay) in [(500, 1), (2000, 1), (2000, 1800)] {
        let mut t = Trajectory::default();
        t.primary(&body(), &terminal(input, input, 0, 0), "route", 100, true);
        let before = t.snapshot();
        let mut appended = body();
        appended["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"append"}));
        t.primary(
            &appended,
            &terminal(input + 100, 0, 0, 0),
            "route",
            100 + delay,
            true,
        );
        let cached = if delay < 1800 { input } else { 0 };
        let expected = scenario_input_cost("gpt-6.1-sol", input + 100, cached, Some(1024)).unwrap();
        assert!((t.snapshot().traditional_cost - before.traditional_cost - expected).abs() < 1e-12);
        assert_eq!(state(&t)["receipts"]["last_appended_input"], 100);
    }
}

#[test]
fn receipt_subscription_traditional_cache_uses_model_policy_not_transport_capability() {
    use carry::models::{Route, resolve_model};

    let model = "gpt-6.1-sol";
    for (endpoint, subscription) in [
        ("https://api.openai.com/v1/responses", false),
        ("https://chatgpt.com/backend-api/codex/responses", true),
        ("http://127.0.0.1:12345/responses", true),
        ("https://example.invalid/responses", false),
    ] {
        let resolved =
            resolve_model(model, Route::new(endpoint, subscription), None, None, 0, 0).unwrap();
        let route_cache = resolved.cache;
        let mut t = Trajectory::default();
        // A real hit does not warm the hypothetical branch's first request.
        t.primary_resolved(
            &body(),
            &terminal(2000, 2000, 0, 0),
            endpoint,
            100,
            true,
            Some(&resolved),
        );
        let cold = t.snapshot();
        let expected_cold = scenario_input_cost(model, 2000, 0, Some(1024)).unwrap();
        assert!(
            (cold.traditional_cost - expected_cold).abs() < 1e-12,
            "{endpoint}: model-level cache economics must not be disabled by transport uncertainty"
        );
        let mut t: Trajectory = serde_json::from_value(state(&t)).unwrap();
        let mut appended = body();
        appended["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"append"}));
        // A real miss does not remove the alternative's conditional prefix reuse.
        t.primary_resolved(
            &appended,
            &terminal(2100, 0, 0, 0),
            endpoint,
            101,
            true,
            Some(&resolved),
        );
        let warm = t.snapshot();
        let expected_warm = scenario_input_cost(model, 2100, 2000, Some(1024)).unwrap();
        assert!((warm.traditional_cost - cold.traditional_cost - expected_warm).abs() < 1e-12);
        assert_eq!(warm.coverage.unknown_cache_requests, 0);
        assert_eq!(
            resolved.cache, route_cache,
            "scenario policy is not a route entitlement"
        );
        assert!((warm.observed_primary_cost - 0.0044).abs() < 1e-12);
        assert_eq!(warm.carry_cost, warm.observed_primary_cost);
    }
}

#[test]
fn receipt_carry_only_rewrite_preserves_traditional_cache_after_restart() {
    let mut t = Trajectory::default();
    t.primary(&body(), &terminal(2000, 1500, 0, 0), "route", 100, true);
    let before = t.snapshot();
    t.compacted(1500, 500);
    assert!(state(&t)["receipts"]["anchor"].is_null());
    let mut t: Trajectory = serde_json::from_value(state(&t)).unwrap();
    let rewritten =
        json!({"model":"gpt-6.1-sol", "input":[{"role":"user","content":"Carry memory"}]});
    t.primary(&rewritten, &terminal(1100, 0, 0, 0), "route", 101, true);
    let after = t.snapshot();
    let expected = scenario_input_cost("gpt-6.1-sol", 2100, 2000, Some(1024)).unwrap();
    assert!(
        (after.traditional_cost - before.traditional_cost - expected).abs() < 1e-12,
        "Carry's edit must not cold-price the untouched traditional prefix"
    );
    assert!(state(&t)["receipts"]["last_appended_input"].is_null());
    assert_eq!(state(&t)["restored"], 1000);
}

#[test]
fn receipt_pending_preserves_completed_costs_and_aggregate_scope() {
    let mut t = Trajectory::default();
    t.primary(&body(), &terminal(2000, 1500, 0, 0), "route", 100, true);
    let settled = serde_json::to_value(t.snapshot().dashboard(false)).unwrap();
    let mut totals = DashboardTotals::default();
    for _ in 0..3 {
        let pending = t.snapshot().dashboard(true);
        let public = serde_json::to_value(&pending).unwrap();
        for field in [
            "carry_cost",
            "traditional_cost",
            "savings",
            "observed_primary_cost",
        ] {
            assert_eq!(
                public[field], settled[field],
                "pending work must not censor completed {field}"
            );
        }
        totals = DashboardTotals::default();
        totals.observe(&pending);
    }
    let aggregate = serde_json::to_value(totals).unwrap();
    assert_eq!(aggregate["included_sessions"], 1);
    assert_eq!(aggregate["carry_cost"], settled["carry_cost"]);
    let empty = serde_json::to_value(Trajectory::default().snapshot().dashboard(true)).unwrap();
    assert!(empty["carry_cost"].is_null());
}

#[test]
fn receipt_unmodeled_actuals_are_unknown_and_coverage_reconciles() {
    let mut t = Trajectory::default();
    let r = terminal(2000, 1000, 0, 10);
    t.primary(&body(), &r, "route", 100, true);
    let known = t.snapshot();
    t.primary(&body(), &r, "route", 101, false);
    t.reviewer(&body(), &r, false);
    t.primary(&body(), &terminal(2100, 0, 0, 0), "route", 102, true);
    let public = serde_json::to_value(t.snapshot().dashboard(false)).unwrap();
    assert_eq!(public["matched_primary_responses"], 2);
    assert_eq!(public["unavailable_primary_cost_calls"], 1);
    assert_eq!(public["unavailable_reviewer_cost_calls"], 1);
    assert_eq!(public["excluded_unpriced_calls"], 2);
    assert!(public["observed_reviewer_cost"].is_null());
    assert!(t.snapshot().observed_primary_cost > known.observed_primary_cost);
    assert_eq!(t.snapshot().observed_primary_cost, t.snapshot().carry_cost);
    let mut totals = DashboardTotals::default();
    totals.observe(&t.snapshot().dashboard(false));
    let aggregate = serde_json::to_value(totals).unwrap();
    assert_eq!(aggregate["matched_primary_responses"], 2);
    assert_eq!(
        aggregate["observed_primary_cost"],
        public["observed_primary_cost"]
    );
    assert_eq!(aggregate["unavailable_primary_cost_calls"], 1);
    assert_eq!(aggregate["unavailable_reviewer_cost_calls"], 1);
    assert!(aggregate["observed_reviewer_cost"].is_null());
}

#[test]
fn receipt_same_prompt_usage_drift_does_not_invent_an_appended_block() {
    let mut t = Trajectory::default();
    t.primary(&body(), &terminal(2000, 0, 0, 0), "route", 100, true);
    t.primary(&body(), &terminal(2100, 0, 0, 0), "route", 101, true);
    assert!(state(&t)["receipts"]["last_appended_input"].is_null());
    assert!(state(&t)["receipts"]["epochs"].as_u64().unwrap() > 0);
    assert_eq!(t.snapshot().matched_primary_responses, 2);
}

#[test]
fn receipt_unknown_actuals_are_null_and_persistence_declares_its_version() {
    let mut t = Trajectory::default();
    t.unmodeled_call();
    let public = serde_json::to_value(t.snapshot().dashboard(false)).unwrap();
    assert!(public["observed_primary_cost"].is_null());
    assert_eq!(public["unavailable_primary_cost_calls"], 1);
    assert_eq!(
        state(&t)["result"]["accounting_basis"],
        "receipt_anchored_v2"
    );
}

#[test]
fn receipt_fixed_body_changes_both_branches_and_both_actual_lanes() {
    for input in [2000, 272_001] {
        let mut t = Trajectory::default();
        t.scenario.context_window = 1_000_000;
        let r = terminal(input, 1000, 100, 10);
        t.primary(&body(), &r, "route", 100, true);
        let primary = t.snapshot();
        let expected = modeled_cost(
            "gpt-6.1-sol",
            &Usage {
                input_tokens: input,
                cached_input_tokens: 1000,
                cache_write_input_tokens: 100,
                output_tokens: 10,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
        assert!(
            (primary.carry_cost - expected).abs() < 1e-12,
            "actual primary must use native partitions"
        );
        assert!(
            (primary.traditional_cost
                - scenario_input_cost("gpt-6.1-sol", input, 0, Some(1024)).unwrap()
                - scenario_output_cost_resolved("gpt-6.1-sol", input, 10, None).unwrap())
            .abs()
                < 1e-12,
            "the counterfactual must share the native input basis, not unchanged JSON tokens"
        );
        t.reviewer(&body(), &r, true);
        assert!(
            (t.snapshot().reviewer_cost - expected).abs() < 1e-12,
            "actual reviewer must use native partitions too"
        );
        assert!((t.snapshot().carry_cost - 2.0 * expected).abs() < 1e-12);
    }
}

#[test]
fn receipt_append_delta_uses_total_input_not_cache_or_hidden_output() {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 3000,
            reserve: 100,
            keep_recent: 100,
            summary: 20,
        },
        ..Trajectory::default()
    };
    let mut b = body();
    t.primary(&b, &terminal(2000, 1000, 0, 100_000), "route", 100, true);
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"whole new block"}));
    t.primary(&b, &terminal(2100, 0, 0, 0), "route", 101, true);
    assert_eq!(
        t.snapshot().traditional_compactions,
        0,
        "billed hidden output is not retained context"
    );
    assert_eq!(
        state(&t)["receipts"]["last_appended_input"],
        100,
        "native output includes hidden reasoning; only the next total input measures the block"
    );
    let mut resumed: Trajectory = serde_json::from_slice(&serde_json::to_vec(&t).unwrap()).unwrap();
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"next block"}));
    resumed.primary(&b, &terminal(2150, 2000, 0, 0), "route", 102, true);
    assert_eq!(state(&resumed)["receipts"]["last_appended_input"], 50);
    let same_total = state(&resumed)["receipts"]["anchor"]["input"].clone();
    resumed.primary(&b, &terminal(2150, 0, 100, 0), "route", 103, true);
    assert_eq!(state(&resumed)["receipts"]["last_appended_input"], 0);
    assert_eq!(state(&resumed)["receipts"]["anchor"]["input"], same_total);
}

#[test]
fn receipt_discontinuities_never_attribute_a_removal_or_sibling_suffix() {
    for variant in [
        "edit",
        "tools",
        "instructions",
        "model",
        "route",
        "rewind",
        "negative",
        "missing",
        "memory",
    ] {
        let mut t = Trajectory::default();
        let mut b = body();
        t.primary(&b, &terminal(2000, 0, 0, 0), "route", 100, true);
        b["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"suffix"}));
        let mut r = terminal(2100, 0, 0, 0);
        match variant {
            "edit" => b["input"][0]["content"] = json!("edited"),
            "tools" => b["tools"] = json!([{"type":"function","name":"new_tool"}]),
            "instructions" => b["instructions"] = json!("new direction"),
            "model" => b["model"] = json!("gpt-6-luna"),
            "rewind" => b["input"] = json!([]),
            "negative" => r = terminal(1900, 0, 0, 0),
            "missing" => r["usage"] = Value::Null,
            "memory" => t.compacted(10, 20),
            _ => (),
        }
        t.primary(
            &b,
            &r,
            if variant == "route" { "other" } else { "route" },
            101,
            true,
        );
        let receipts = &state(&t)["receipts"];
        assert!(receipts.is_object());
        assert!(receipts["last_appended_input"].is_null(), "{variant}");
        if variant == "missing" {
            assert!(receipts["anchor"].is_null());
        }
        let mut fork = t.clone();
        fork.reset_epoch();
        fork.primary(&body(), &terminal(2200, 0, 0, 0), "route", 102, true);
        assert!(
            state(&fork)["receipts"]["last_appended_input"].is_null(),
            "fork must start cold"
        );
    }
}

#[test]
fn receipt_bounded_anchor_restart_and_legacy_window_do_not_reprice_history() {
    let mut t = Trajectory::default();
    let mut b = body();
    for n in 0..32 {
        b["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"private-canary ".repeat(1000)}));
        t.primary(&b, &terminal(2000 + n, 0, 0, 0), "route", 100 + n, true);
    }
    let receipt = state(&t)["receipts"].clone();
    assert!(
        serde_json::to_vec(&receipt).unwrap().len() < 2048,
        "one bounded numeric anchor, no prompt archive"
    );
    assert!(!receipt.to_string().contains("private-canary"));
    let mut legacy = state(&t);
    legacy.as_object_mut().unwrap().remove("receipts");
    let mut old: Trajectory = serde_json::from_value(legacy.clone()).unwrap();
    let before = old.snapshot();
    let old_public = serde_json::to_value(old.snapshot().dashboard(false)).unwrap();
    assert_eq!(old_public["accounting_basis"], "legacy_json_v1");
    assert_eq!(
        old_public["counterfactual_basis"],
        "legacy_local_json_projection"
    );
    let mut totals = DashboardTotals::default();
    totals.observe(&old.snapshot().dashboard(false));
    totals.observe(&t.snapshot().dashboard(false));
    let current = serde_json::to_value(t.snapshot().dashboard(false)).unwrap();
    assert_eq!(current["accounting_basis"], "receipt_anchored_v2");
    let total = serde_json::to_value(totals).unwrap();
    assert_eq!(total["included_sessions"], 1);
    assert_eq!(total["legacy_sessions"], 1);
    assert_eq!(total["carry_cost"], current["carry_cost"]);
    old.begin_receipt_window();
    old.primary(&body(), &terminal(100_000, 0, 0, 3), "route", 200, true);
    let public = serde_json::to_value(old.snapshot().dashboard(false)).unwrap();
    assert_eq!(public["accounting_basis"], "receipt_anchored_v2");
    assert_eq!(
        public["legacy_estimate"]["carry_cost"], before.carry_cost,
        "legacy sums remain separate, never repriced or blended"
    );
    assert_eq!(public["matched_primary_responses"], 1);
}

#[test]
fn receipt_missing_conflicting_and_failed_usage_remain_unknown_not_projected() {
    for variant in [
        "no_cache",
        "overlap",
        "conflict",
        "null_write",
        "invalid_write",
        "missing",
        "failed",
    ] {
        let mut t = Trajectory::default();
        let mut r = terminal(2000, 1000, 100, 10);
        match variant {
            "no_cache" => {
                r["usage"]["input_tokens_details"]
                    .as_object_mut()
                    .unwrap()
                    .remove("cached_tokens");
            }
            "overlap" => r["usage"]["input_tokens_details"]["cache_write_tokens"] = json!(1500),
            "conflict" => r["usage"]["input_tokens_details"]["cache_creation_tokens"] = json!(200),
            "null_write" => r["usage"]["input_tokens_details"]["cache_write_tokens"] = Value::Null,
            "invalid_write" => {
                r["usage"]["input_tokens_details"]["cache_write_tokens"] = json!("100")
            }
            "missing" => r["usage"] = Value::Null,
            _ => r["status"] = json!("failed"),
        }
        t.primary(&body(), &r, "route", 100, true);
        t.reviewer(&body(), &r, true);
        assert!(
            !t.snapshot().priced,
            "{variant}: must not fabricate a matched comparison"
        );
        assert_eq!(t.snapshot().carry_cost, 0.0, "{variant}");
        assert_eq!(t.snapshot().traditional_cost, 0.0, "{variant}");
    }
}
