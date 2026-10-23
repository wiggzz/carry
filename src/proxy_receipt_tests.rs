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
fn receipt_fixed_body_changes_both_branches_and_both_actual_lanes() {
    for input in [2000, 272_001] {
        let mut t = Trajectory::default();
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
    let mut t = Trajectory::default();
    let mut b = body();
    t.primary(&b, &terminal(2000, 1000, 0, 100_000), "route", 100, true);
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"whole new block"}));
    t.primary(&b, &terminal(2100, 0, 0, 0), "route", 101, true);
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
    let mut totals = DashboardTotals::default();
    totals.observe(&old.snapshot().dashboard(false));
    totals.observe(&t.snapshot().dashboard(false));
    let current = serde_json::to_value(t.snapshot().dashboard(false)).unwrap();
    assert_eq!(current["accounting_basis"], "receipt_anchored_v2");
    let total = serde_json::to_value(totals).unwrap();
    assert_eq!(total["included_sessions"], 1);
    assert_eq!(total["legacy_sessions"], 1);
    assert_eq!(total["carry_cost"], current["carry_cost"]);
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
    for variant in ["no_cache", "overlap", "conflict", "missing", "failed"] {
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
