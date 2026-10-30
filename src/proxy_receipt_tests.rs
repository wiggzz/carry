//! Synthetic receipts: the wire body deliberately does not predict provider units.
use super::*;
include!("proxy_cache_write_tests.rs");
include!("primary_parity_tests.rs");

// Output allowance changes feasibility, not the native-input history contract.
// Exercise both existing initialization and a real in-window paid summary.
fn output_allowance_preserves_history(initialized: bool) {
    let mut t = Trajectory::default();
    let mut request = body();
    request["max_output_tokens"] = json!(1);
    if !initialized {
        t.primary(&request, &terminal(10000, 0, 0, 0), "route", 99, true);
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"grow"}));
    }
    for (offset, input) in [300000, 300100, 300200].into_iter().enumerate() {
        if offset > 0 {
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("append {offset}")}));
        }
        t.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "route",
            100 + offset as u64,
            true,
        );
    }
    assert_eq!(t.history.retired, 280000);
    assert_eq!(t.history.summary, 1000);
    assert_eq!(t.history.last_traditional_input, 21200);
    let before = t.snapshot();
    assert_eq!(before.initial_compactions, u64::from(initialized));
    assert_eq!(before.traditional_compactions, u64::from(!initialized));
    assert_eq!(before.summary_cost > 0.0, !initialized);
    t = serde_json::from_value(state(&t)).unwrap();
    request["max_output_tokens"] = json!(2);
    for (offset, input) in [300300, 300400].into_iter().enumerate() {
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":format!("new allowance {offset}")}));
        t.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "route",
            103 + offset as u64,
            true,
        );
        let after = t.snapshot();
        assert!(
            (after.observed_primary_cost
                - before.observed_primary_cost
                - (0..=offset)
                    .map(|n| (300300 + n * 100) as f64 * 4.0 / 1e6)
                    .sum::<f64>())
            .abs()
                < 1e-12
        );
        assert_eq!(
            after.summary_cost, before.summary_cost,
            "changing output allowance cannot charge another summary"
        );
        assert_eq!(
            after.traditional_compactions,
            before.traditional_compactions
        );
        assert_eq!(
            after.initial_compactions, before.initial_compactions,
            "changing output allowance cannot grant another free initialization"
        );
        assert_eq!(
            t.history.retired, 280000,
            "retain the original retired quantity"
        );
        assert_eq!(t.history.summary, 1000);
        assert_eq!(
            t.history.last_traditional_input,
            21300 + offset as u64 * 100,
            "retain every appended native-input token"
        );
        t = serde_json::from_value(state(&t)).unwrap();
    }
}

// Review4424550949: input instruction roles are fixed, not old chat history.
fn fixed_input_role_comparison(role: &str) {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 4000,
            reserve: 100,
            keep_recent: 1500,
            summary: 100,
        },
        ..Trajectory::default()
    };
    let mut request = json!({"model":"gpt-6.1-sol", "max_output_tokens":1,
        "input":[{"role":role,"content":" fixed".repeat(5000)},
                 {"role":"user","content":"current request"}]});
    for index in 0..2 {
        if index > 0 {
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":"continue"}));
        }
        let before = t.snapshot();
        let input = 5100 + index * 100;
        t.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "fixed-role",
            100 + index,
            true,
        );
        let result = t.snapshot();
        assert!(
            (result.observed_primary_cost
                - before.observed_primary_cost
                - input as f64 * 2.0 / 1e6)
                .abs()
                < 1e-12
        );
        assert_eq!(result.traditional_compactions, 0);
        assert_eq!(result.summary_cost, 0.0);
        if role == "assistant" {
            assert_eq!(result.initial_compactions, 1);
            assert_eq!(result.matched_primary_responses, index + 1);
            assert!(result.priced);
        } else {
            assert_eq!(
                result.matched_primary_responses, 0,
                "FIXED_INPUT_ROLE: instruction messages cannot be hypothetically retired"
            );
            assert_eq!(result.initial_compactions, 0);
            assert!(!result.priced);
        }
        t = serde_json::from_value(state(&t)).unwrap();
    }
}
#[test]
fn fixed_input_system_role_cannot_be_retired_across_restart() {
    fixed_input_role_comparison("system");
}
#[test]
fn fixed_input_developer_role_cannot_be_retired_across_restart() {
    fixed_input_role_comparison("developer");
}
#[test]
fn fixed_input_assistant_history_can_be_retired_across_restart() {
    fixed_input_role_comparison("assistant");
}

#[test]
fn history_contract_output_allowance_preserves_free_initialized_growth() {
    output_allowance_preserves_history(true);
}

#[test]
fn history_contract_output_allowance_preserves_paid_summary_growth() {
    output_allowance_preserves_history(false);
}

// Exercise both free pre-window initialization (#186) and an in-window paid
// summary after a small recognized prompt; migration preserves either history.
#[test]
fn receipt_policy_migration_preserves_summary_with_exact_legacy_contract() {
    use carry::models::{Route, resolve_model};
    for version in [None, Some(1), Some(2)] {
        for (projected, initialized) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut prior = resolve_model(
                "gpt-6.1-sol",
                Route::new("https://api.openai.com/v1/responses", false),
                None,
                None,
                0,
                0,
            )
            .unwrap();
            if let Some(version) = version {
                prior.scenario_cache_policy.as_mut().unwrap().version = version;
                prior.provenance.get_mut("scenario_cache").unwrap().revision =
                    "old-frozen-policy".into();
            } else {
                prior.scenario_cache_policy = None;
                prior.provenance.remove("scenario_cache");
            }
            let mut current = prior.clone().with_scenario_cache_policy();
            if version == Some(2) {
                current.scenario_cache_policy.as_mut().unwrap().version = 3;
            }
            let mut t = Trajectory {
                scenario: Scenario {
                    context_window: 4000,
                    reserve: 100,
                    keep_recent: 1500,
                    summary: 500,
                },
                ..Trajectory::default()
            };
            let mut b = body();
            b["max_output_tokens"] = json!(1);
            if !initialized {
                t.primary_resolved(
                    &b,
                    &terminal(1000, 0, 0, 0),
                    "route",
                    99,
                    true,
                    Some(&prior),
                );
                b["input"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"role":"user","content":"grow"}));
            }
            t.primary_resolved(
                &b,
                &terminal(5000, 0, 0, 0),
                "route",
                100,
                true,
                Some(&prior),
            );
            assert_eq!(t.history.retired, 3500);
            assert_eq!(t.history.summary, 500);
            let before = t.snapshot();
            assert_eq!(before.initial_compactions, u64::from(initialized));
            assert_eq!(before.traditional_compactions, u64::from(!initialized));
            assert_eq!(before.summary_cost > 0.0, !initialized);
            let mut legacy = state(&t);
            // Preserve the original exact-old-contract migration control even
            // though new successful windows now persist a structured cursor.
            legacy.as_object_mut().unwrap().remove("history_continuity");
            legacy["receipts"]["history_anchor"] = legacy["receipts"]["anchor"].clone();
            t = serde_json::from_value(legacy).unwrap();
            b["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":"append"}));
            if projected {
                t.projected(5100, 3000, true);
            }
            t.rekey_receipt_policy(&b, "route", &prior, &current);
            t.primary_resolved(
                &b,
                &terminal(if projected { 3000 } else { 5100 }, 0, 0, 0),
                "route",
                101,
                true,
                Some(&current),
            );
            let after = t.snapshot();
            assert_eq!(
                after.traditional_compactions,
                u64::from(!initialized),
                "{version:?}, projection={projected}"
            );
            assert_eq!(after.initial_compactions, u64::from(initialized));
            assert_eq!(after.summary_cost, before.summary_cost);
            assert_eq!(t.history.retired, 3500);
            assert_eq!(t.history.summary, 500);
            assert_eq!(t.history.last_traditional_input, 2100);
            let cold =
                scenario_input_cost_resolved("gpt-6.1-sol", 2100, 0, Some(1024), Some(&current))
                    .unwrap();
            assert!((after.traditional_cost - before.traditional_cost - cold).abs() < 1e-12);
            let boundaries = &after.cache_policy_boundaries;
            let boundary = boundaries.last().unwrap();
            assert_eq!(boundary.previous_version, version);
            assert_eq!(
                boundary.next_version,
                current.scenario_cache_policy.as_ref().map(|p| p.version)
            );
            assert_eq!(
                boundary.cumulative.traditional_cost,
                before.traditional_cost
            );
            assert_eq!(boundary.cumulative.summary_cost, before.summary_cost);
            t = serde_json::from_value(state(&t)).unwrap();
            b["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":"warm"}));
            t.primary_resolved(
                &b,
                &terminal(if projected { 3100 } else { 5200 }, 0, 0, 0),
                "route",
                102,
                true,
                Some(&current),
            );
            let warm =
                scenario_input_cost_resolved("gpt-6.1-sol", 2200, 2100, Some(1024), Some(&current))
                    .unwrap();
            assert!((t.snapshot().traditional_cost - after.traditional_cost - warm).abs() < 1e-12);
            assert_eq!(
                t.snapshot().traditional_compactions,
                u64::from(!initialized)
            );
            assert_eq!(t.snapshot().initial_compactions, u64::from(initialized));
            assert_eq!(
                serde_json::to_value(t.snapshot().cache_policy_boundaries).unwrap(),
                serde_json::to_value(boundaries).unwrap()
            );
        }
    }
}

#[test]
fn receipt_policy_migration_does_not_bypass_trajectory_guards() {
    use carry::models::{Route, resolve_model};
    for change in [
        "instructions",
        "tools",
        "route",
        "auth",
        "tier",
        "rates",
        "edit",
        "shorter",
        "clock",
        "input",
        "missing_proof",
        "reset",
    ] {
        let prior = resolve_model(
            "gpt-6.1-sol",
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            0,
            0,
        )
        .unwrap();
        let mut current = prior.clone();
        current.scenario_cache_policy.as_mut().unwrap().version = 3;
        let mut t = Trajectory {
            scenario: Scenario {
                context_window: 4000,
                reserve: 100,
                keep_recent: 1500,
                summary: 500,
            },
            ..Trajectory::default()
        };
        let mut b = body();
        b["max_output_tokens"] = json!(1);
        t.primary_resolved(
            &b,
            &terminal(5000, 0, 0, 0),
            "route",
            100,
            true,
            Some(&prior),
        );
        t = serde_json::from_value(state(&t)).unwrap();
        b["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"append"}));
        let mut route = "route";
        let mut at = 101;
        let mut input = 5100;
        match change {
            "instructions" => b["instructions"] = json!("new"),
            "tools" => b["tools"] = json!([{"type":"function","name":"new"}]),
            "route" => route = "other",
            "auth" => current.route = Route::new("https://api.openai.com/v1/responses", true),
            "tier" => current.route.processing_tier = "priority".into(),
            "rates" => current.rates.as_mut().unwrap().input *= 2.0,
            "edit" => b["input"][0]["content"] = json!("edited ancestor"),
            "shorter" => b["input"] = json!([]),
            "clock" => at = 99,
            "input" => input = 4900,
            "missing_proof" => {
                let mut saved = state(&t);
                // This old-decoder control must actually select legacy state;
                // a typed cursor no longer depends on the opaque cache digest.
                saved.as_object_mut().unwrap().remove("history_continuity");
                saved["receipts"]["history_anchor"] = saved["receipts"]["anchor"].clone();
                saved["receipts"]["configuration"] = json!("mismatched");
                t = serde_json::from_value(saved).unwrap();
            }
            "reset" => t.reset_epoch(),
            _ => unreachable!(),
        }
        t.rekey_receipt_policy(&b, route, &prior, &current);
        t.primary_resolved(
            &b,
            &terminal(input, 0, 0, 0),
            route,
            at,
            true,
            Some(&current),
        );
        assert_eq!(
            t.snapshot().initial_compactions,
            if matches!(change, "rates" | "tier") {
                1
            } else {
                2
            },
            "{change}: pricing is not a history contract; legacy/ancestry guards remain"
        );
        if matches!(change, "rates" | "tier") {
            assert_eq!(t.history.retired, 3500);
            assert_eq!(t.history.summary, 500);
            assert_eq!(t.history.last_traditional_input, 2100);
        }
    }
}

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

// Tests-only follow-up to the output-aware initialization repair. An unchanged
// image receipt is priceable, but it cannot allocate native tokens to a slice.
#[test]
fn initialization186_output_trigger_cannot_compact_unallocated_image_input() {
    for cap in [1, 100000] {
        let request = json!({"model":"gpt-6.1-sol", "max_output_tokens":cap,
            "input":[{"role":"assistant", "content":" evidence".repeat(200000)},
                {"role":"user", "content":[
                    {"type":"input_text", "text":"current image request"},
                    {"type":"input_image", "image_url":"data:image/png;base64,fixture"}]}]});
        let mut t = Trajectory::default();
        t.primary(&request, &terminal(201000, 0, 0, 0), "route", 100, true);
        let result = t.snapshot();
        assert!((result.observed_primary_cost - 0.402).abs() < 1e-12);
        assert_eq!(result.unavailable_primary_cost_calls, 0);
        assert_eq!(result.coverage.primary_responses, 1);
        assert_eq!(
            result.initial_compactions, 0,
            "a projected image slice has no measured native token allocation"
        );
        assert_eq!(result.traditional_compactions, 0);
        if cap == 1 {
            assert_eq!(result.coverage.unmodeled_calls, 0);
            assert_eq!(result.matched_primary_responses, 1);
            assert!((result.traditional_cost - result.observed_primary_cost).abs() < 1e-12);
        } else {
            assert_eq!(result.coverage.unmodeled_calls, 1);
            assert_eq!(result.excluded_unpriced_calls, 1);
            assert_eq!(result.matched_primary_responses, 0);
        }
    }
}

#[test]
fn initialization186_resolved_hard_capacity_bounds_the_counterfactual() {
    use carry::models::{Route, resolve_model};
    let resolved = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        Some(50000),
        0,
        0,
    )
    .unwrap();
    assert_eq!(resolved.capacity_tokens, Some(50000));
    for cap in [1, 40000] {
        // Receipt-domain tracer, not a realistic oversized wire or provider
        // measurement. The actual receipt stays paid even if comparison fails.
        let request = json!({"model":"gpt-6.1-sol", "max_output_tokens":cap,
            "input":[{"role":"assistant","content":"old synthetic history"},
                     {"role":"user","content":"current"}]});
        let mut t = Trajectory::default();
        t.primary_resolved(
            &request,
            &terminal(60000, 0, 0, 0),
            "route",
            100,
            true,
            Some(&resolved),
        );
        let result = t.snapshot();
        assert!((result.observed_primary_cost - 0.12).abs() < 1e-12);
        assert_eq!(result.unavailable_primary_cost_calls, 0);
        assert_eq!(result.summary_cost, 0.0);
        assert_eq!(result.traditional_compactions, 0);
        if cap == 1 {
            assert_eq!(
                result.initial_compactions, 1,
                "a prompt below economic headroom but above hard capacity must initialize first"
            );
            assert_eq!(result.matched_primary_responses, 1);
            assert!((result.traditional_cost - 0.0525).abs() < 1e-12);
            assert!(t.history.last_traditional_input + cap <= 50000);
        } else {
            assert_eq!(result.initial_compactions, 0);
            assert_eq!(
                result.matched_primary_responses, 0,
                "retained prompt plus requested output cannot fit the resolved hard capacity"
            );
            assert_eq!(result.excluded_unpriced_calls, 1);
        }
    }
}

#[test]
fn initialization186_omitted_output_uses_frozen_capacity_reserve() {
    use carry::models::{Route, resolve_model};
    for (cap, reserve) in [(Some(1_u64), 40000_u32), (None, 16384), (None, 40000)] {
        let resolved = resolve_model(
            "gpt-6.1-sol",
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            Some(50000),
            reserve,
            10,
        )
        .unwrap();
        assert_eq!(resolved.capacity_tokens, Some(50000));
        assert_eq!(resolved.ordinary.output_reserve, reserve);
        let mut request = json!({"model":"gpt-6.1-sol",
            "input":[{"role":"assistant","content":"old synthetic history"},
                     {"role":"user","content":"current"}]});
        if let Some(cap) = cap {
            request["max_output_tokens"] = json!(cap);
        }
        let mut t = Trajectory::default();
        t.primary_resolved(
            &request,
            &terminal(40000, 0, 0, 0),
            "route",
            100,
            true,
            Some(&resolved),
        );
        let result = t.snapshot();
        assert!((result.observed_primary_cost - 0.08).abs() < 1e-12);
        assert_eq!(result.unavailable_primary_cost_calls, 0);
        assert_eq!(result.summary_cost, 0.0);
        assert_eq!(result.traditional_compactions, 0);
        if cap.is_some() {
            assert_eq!(result.initial_compactions, 0);
            assert_eq!(result.matched_primary_responses, 1);
            assert!((result.traditional_cost - result.observed_primary_cost).abs() < 1e-12);
        } else if reserve == 16384 {
            assert_eq!(
                result.initial_compactions, 1,
                "omitted output must reserve frozen configured allowance"
            );
            assert_eq!(result.matched_primary_responses, 1);
            assert!((result.traditional_cost - 0.0525).abs() < 1e-12);
            assert!(t.history.last_traditional_input + u64::from(reserve) <= 50000);
        } else {
            assert_eq!(result.initial_compactions, 0);
            assert_eq!(
                result.matched_primary_responses, 0,
                "retained floor plus fallback output cannot fit"
            );
            assert_eq!(result.excluded_unpriced_calls, 1);
        }
    }
}

// Joined #191/#192 acceptance, not a separately reproduced defect. Native
// execution is pending the rebuilt joined-source hosted lane.
#[test]
fn joined191192_image_receipts_obey_write_lifetime_without_erasing_known_money() {
    for breakpoint in [false, true] {
        let mut t = Trajectory::default();
        let mut expected = 0.0;
        for (index, (ttl, written, priced)) in [
            ("30m", 2000, true),
            ("12h", 2000, false),
            ("12h", 0, true),
            ("30m", 2000, true),
        ]
        .into_iter()
        .enumerate()
        {
            let mut request = body();
            request["prompt_cache_options"] = json!({"mode":"implicit","ttl":"30m"});
            request["input"][0]["content"] = json!([
                {"type":"input_text","text":"image source"},
                {"type":"input_image","image_url":"data:image/png;base64,fixture"}
            ]);
            if breakpoint {
                request["input"][0]["content"][1]["prompt_cache_breakpoint"] =
                    json!({"mode":"explicit","ttl":ttl});
            } else {
                request["prompt_cache_options"]["ttl"] = json!(ttl);
            }
            t.primary(
                &request,
                &terminal(4000, 1000, written, 5),
                "joined-image-fixture",
                100 + index as u64,
                true,
            );
            if priced {
                expected += ((3000 - written) as f64 * 2.0
                    + 1000.0 * 0.1
                    + written as f64 * 2.5
                    + 5.0 * 10.0)
                    / 1e6;
            }
            let snapshot = t.snapshot();
            assert!((snapshot.observed_primary_cost - expected).abs() < 1e-12);
            assert_eq!(
                snapshot.unavailable_primary_cost_calls,
                u64::from(index > 0)
            );
            assert_eq!(snapshot.coverage.unmodeled_calls, 0);
            assert_eq!(
                snapshot.matched_primary_responses,
                if breakpoint || index < 3 { 1 } else { 2 }
            );
            t = serde_json::from_value(state(&t)).unwrap();
        }
    }
}

#[test]
fn receipt_image_input_uses_native_totals_not_encoded_payload_length() {
    let mut costs = Vec::new();
    for payload in ["short".to_owned(), "encoded ".repeat(4000)] {
        let mut request = body();
        request["input"][0]["content"] = json!([
            {"type":"input_text","text":"image source"},
            {"type":"input_image","image_url":format!("data:image/png;base64,{payload}")}
        ]);
        let mut t = Trajectory::default();
        t.primary(&request, &terminal(4000, 1000, 0, 5), "route", 100, true);
        let first = t.snapshot();
        assert_eq!(first.unavailable_primary_cost_calls, 0);
        assert_eq!(first.matched_primary_responses, 1);
        assert_eq!(first.coverage.unmodeled_calls, 0);
        assert_eq!(t.history.last_traditional_input, 4000);
        assert_eq!(t.receipt_prediction(&request, "route", None), Some(4000));
        costs.push(first.observed_primary_cost);
        t = serde_json::from_value(state(&t)).unwrap();
        t.primary(&request, &terminal(4000, 3000, 0, 5), "route", 101, true);
        assert_eq!(t.snapshot().matched_primary_responses, 2);
        assert_eq!(state(&t)["receipts"]["last_appended_input"], 0);
        let mut appended = request.clone();
        appended["input"].as_array_mut().unwrap().push(json!({
            "role":"user","content":[{"type":"input_image","image_url":"new unmeasured image"}]
        }));
        assert_eq!(t.receipt_prediction(&appended, "route", None), None);
        // A changed multimodal projection has no measured allocation for its
        // restored image/material. Keep known expense, omit that comparison.
        t.projected(2000, 1000, true);
        let before = t.snapshot().observed_primary_cost;
        t.primary(&request, &terminal(3000, 0, 0, 5), "route", 102, true);
        let after = t.snapshot();
        assert!(after.observed_primary_cost > before);
        assert_eq!(after.unavailable_primary_cost_calls, 0);
        assert_eq!(after.matched_primary_responses, 2);
        assert_eq!(after.coverage.unmodeled_calls, 1);
        assert_eq!(after.excluded_unpriced_calls, 1);
    }
    assert_eq!(costs[0], costs[1]);
}

#[test]
fn receipt_images_do_not_invent_summary_or_audio_token_tariffs() {
    for kind in ["input_image", "input_audio", "input_video"] {
        let mut request = body();
        request["input"][0]["content"] = json!([{ "type":kind,"data":"opaque" }]);
        let mut t = Trajectory {
            scenario: Scenario {
                context_window: 3000,
                reserve: 100,
                keep_recent: 100,
                summary: 50,
            },
            ..Trajectory::default()
        };
        t.primary(&request, &terminal(4000, 0, 0, 0), "route", 100, true);
        let result = t.snapshot();
        assert_eq!(result.matched_primary_responses, 0);
        assert_eq!(result.traditional_compactions, 0);
        assert_eq!(result.coverage.unmodeled_calls, 1);
        assert_eq!(
            result.unavailable_primary_cost_calls,
            u64::from(kind != "input_image")
        );
        assert_eq!(result.observed_primary_cost > 0.0, kind == "input_image");
    }
}

#[test]
fn changed_receipt_configuration_preserves_signed_current_projection_not_old_summary() {
    for difference in [1000_i128, -200, 0] {
        let mut t = Trajectory::default();
        let mut request = body();
        request["max_output_tokens"] = json!(1);
        t.projected(5000, (5000_i128 - difference) as u64, difference != 0);
        t.primary(&request, &terminal(3000, 0, 0, 0), "route", 100, true);
        let paid = t.snapshot().observed_primary_cost;
        t = serde_json::from_value(state(&t)).unwrap();
        request["max_output_tokens"] = json!(2);
        t.projected(5000, (5000_i128 - difference) as u64, difference != 0);
        t.primary(&request, &terminal(3000, 0, 0, 0), "route", 101, true);
        assert_eq!(
            state(&t)["restored"],
            json!(difference),
            "a receipt epoch is not the lifetime of the current projection"
        );
        assert_eq!(
            state(&t)["last_traditional_input"],
            json!(3000_i128 + difference)
        );
        assert!(t.snapshot().observed_primary_cost > paid);
        assert_eq!(t.snapshot().matched_primary_responses, 2);
    }
}

#[test]
fn receipt_current_projection_repeats_changes_and_retains_without_accumulation() {
    let mut projected = Trajectory {
        scenario: Scenario {
            context_window: 3600,
            reserve: 100,
            keep_recent: 100,
            summary: 50,
        },
        ..Trajectory::default()
    };
    let mut retained = projected.clone();
    // Both controls forecast a distinct history; the latest zero projection
    // does not prove their previous executions were equal to the paid path.
    retained.projected(100, 100, true);
    // Two copies of the original 1000-unit omission would cross 3500;
    // neither the real 3000-unit original nor any current projection does.
    for (i, difference) in [1000_i128, 1000, 500, 1500, -200, 0]
        .into_iter()
        .enumerate()
    {
        projected.projected(5000, (5000_i128 - difference) as u64, difference != 0);
        projected.primary(
            &body(),
            &terminal((3000_i128 - difference) as u64, 0, 0, 0),
            "route",
            100 + i as u64,
            true,
        );
        retained.projected(5000, 5000, false);
        retained.primary(
            &body(),
            &terminal(3000, 0, 0, 0),
            "route",
            100 + i as u64,
            true,
        );
        assert_eq!(state(&projected)["restored"], json!(difference));
        assert_eq!(state(&projected)["last_traditional_input"], 3000);
        assert_eq!(projected.snapshot().traditional_compactions, 0);
        assert_eq!(projected.snapshot().summary_cost, 0.0);
        assert!(
            (projected.snapshot().traditional_cost - retained.snapshot().traditional_cost).abs()
                < 1e-12,
            "current omission, partial/grown omission, signed memory growth and canonical retention share the no-omission alternative"
        );
        projected = serde_json::from_value(state(&projected)).unwrap();
    }
    assert_eq!(projected.snapshot().coverage.primary_responses, 6);
    assert_eq!(projected.snapshot().matched_primary_responses, 6);
    assert!(projected.snapshot().observed_primary_cost > 0.0);
}

#[test]
fn receipt_projection_preserves_legitimate_summary_retirement_and_failed_paid_scope() {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 3600,
            reserve: 100,
            keep_recent: 100,
            summary: 50,
        },
        ..Trajectory::default()
    };
    // Establish an in-window trajectory before crossing the summary threshold.
    // An already-oversized unknown first request now initializes for free.
    let mut request = body();
    t.primary(&request, &terminal(3000, 0, 0, 0), "route", 99, true);
    request["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"in-window growth"}));
    t.primary(&request, &terminal(4000, 0, 0, 0), "route", 100, true);
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert_eq!(t.snapshot().traditional_compactions, 1);
    let retired = t.history.retired;
    let summary = t.history.summary;
    for at in [101, 102, 103] {
        t.projected(5000, 4000, true);
        let before = t.snapshot();
        if at == 102 {
            let mut failed = terminal(3500, 0, 0, 0);
            failed["status"] = json!("failed");
            t.primary(&body(), &failed, "route", at, true);
            assert_eq!(t.snapshot().traditional_cost, before.traditional_cost);
            assert_eq!(t.snapshot().carry_cost, before.carry_cost);
            assert_eq!(t.snapshot().coverage.failed_calls, 1);
        }
        t.primary(&body(), &terminal(3500, 0, 0, 0), "route", at, true);
        assert_eq!(t.history.retired, retired);
        assert_eq!(t.history.summary, summary);
        assert_eq!(t.history.last_traditional_input, 4500 - retired + summary);
        assert_eq!(t.snapshot().traditional_compactions, 1);
        t = serde_json::from_value(state(&t)).unwrap();
    }
    assert_eq!(t.snapshot().coverage.primary_responses, 5);
    assert_eq!(t.snapshot().excluded_failed_calls, 1);
}

#[test]
fn receipt_equal_size_projection_edit_invalidates_actual_not_traditional_anchor() {
    let mut t = Trajectory::default();
    t.primary(&body(), &terminal(2000, 0, 0, 0), "route", 100, true);
    t.projected(100, 100, true);
    assert_eq!(t.restored, 0);
    assert!(state(&t)["receipts"]["anchor"].is_null());
    assert!(state(&t)["receipts"]["modeled_cache"]["writes"][0].is_object());
    let mut edited = body();
    edited["input"][0]["content"] = json!("different equal-size synthetic memory");
    t.primary(&edited, &terminal(2000, 0, 0, 0), "route", 101, true);
    assert_eq!(t.history.last_traditional_input, 2000);
    assert!(state(&t)["receipts"]["last_appended_input"].is_null());
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
    t.projected(100_100, 100, true);
    t.primary(
        &body(),
        &terminal(200_000, 1000, 100, 10),
        "route",
        101,
        true,
    );
    let after = t.snapshot();
    let expected = scenario_input_cost("gpt-6.1-sol", 300_000, 200_000, Some(1024)).unwrap()
        + scenario_output_cost_resolved("gpt-6.1-sol", 300_000, 10, None).unwrap();
    assert!(
        (after.traditional_cost - before.traditional_cost - expected).abs() < 1e-12,
        "estimated restoration crosses the traditional tariff threshold only"
    );
    assert!((after.carry_cost - 2.0 * before.carry_cost).abs() < 1e-12);
    assert_eq!(after.observed_primary_cost, after.carry_cost);
    assert!(state(&t)["receipts"]["last_appended_input"].is_null());
    // The new request's complete projection survives a receipt-config reset;
    // only historical anchors/summary assumptions belong to the old epoch.
    t.projected(10_000, 0, true);
    let mut changed = body();
    changed["instructions"] = json!("a new configuration");
    t.primary(&changed, &terminal(200_000, 0, 0, 0), "route", 102, true);
    assert_eq!(state(&t)["restored"], 10_000);
    assert_eq!(state(&t)["last_traditional_input"], 210_000);
}

#[test]
fn receipt_counterfactual_cache_is_conditional_not_copied_from_actual_hits() {
    for (input, delay) in [
        (500, 1),
        (1023, 1),
        (1024, 1),
        (2000, 1),
        (2000, 1799),
        (2000, 1800),
    ] {
        let mut t = Trajectory::default();
        // A genuine equal-token rewrite keeps this a divergent-forecast control.
        t.projected(100, 100, true);
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
        // A genuine equal-token rewrite keeps this a divergent-forecast control.
        t.projected(100, 100, true);
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
    t.projected(1500, 500, true);
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
fn receipt_failed_reviewer_does_not_invalidate_primary_or_traditional_cache() {
    for status in ["failed", "incomplete"] {
        for restart in [false, true] {
            let mut t = Trajectory::default();
            // A genuine equal-token rewrite keeps this a divergent-forecast control.
            t.projected(100, 100, true);
            t.primary(&body(), &terminal(2000, 0, 0, 0), "route", 100, true);
            let before = t.snapshot();
            let mut failed = terminal(1000, 0, 0, 10);
            failed["status"] = json!(status);
            t.reviewer_resolved(&body(), &failed, true, None);
            if restart {
                t = serde_json::from_value(state(&t)).unwrap();
            }
            let mut next = body();
            next["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":"continue"}));
            assert!(t.receipt_prediction(&next, "route", None).is_some());
            t.primary(&next, &terminal(2100, 0, 0, 0), "route", 101, true);
            let after = t.snapshot();
            let expected = scenario_input_cost("gpt-6.1-sol", 2100, 2000, Some(1024)).unwrap();
            assert!((after.traditional_cost - before.traditional_cost - expected).abs() < 1e-12);
            assert_eq!(after.coverage.failed_calls, 1);
            assert_eq!(after.coverage.primary_responses, 2);
            assert_eq!(after.reviewer_cost, 0.0);
        }
    }
}

#[test]
fn receipt_frozen_scenario_policy_and_tariff_survive_restart_without_route_credit() {
    use carry::models::{Route, resolve_model};
    let mut resolved = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://chatgpt.com/backend-api/codex/responses", true),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    assert!(resolved.cache.is_none());
    assert!(!resolved.explicit_cache_transport);
    resolved.rates.as_mut().unwrap().input = 7.0;
    resolved.rates.as_mut().unwrap().write = 9.0;
    resolved.rates.as_mut().unwrap().cached = 0.1;
    let policy = resolved.scenario_cache_policy.as_mut().unwrap();
    policy.minimum_prefix_tokens = Some(1500);
    policy.ttl_seconds = 7;
    let mut t = Trajectory::default();
    // A genuine equal-token rewrite keeps this a divergent-forecast control.
    t.projected(100, 100, true);
    t.primary_resolved(
        &body(),
        &terminal(2000, 1000, 0, 0),
        "route",
        100,
        true,
        Some(&resolved),
    );
    assert!((t.snapshot().traditional_cost - 0.018).abs() < 1e-12);
    let checkpoint = json!({"trajectory":t,"resolved":resolved});
    let mut t: Trajectory = serde_json::from_value(checkpoint["trajectory"].clone()).unwrap();
    let resolved: ResolvedModel = serde_json::from_value(checkpoint["resolved"].clone()).unwrap();
    let mut b = body();
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"next"}));
    t.primary_resolved(
        &b,
        &terminal(2100, 0, 0, 0),
        "route",
        106,
        true,
        Some(&resolved),
    );
    assert!((t.snapshot().traditional_cost - 0.0191).abs() < 1e-12);
    let before = t.snapshot();
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"expired"}));
    t.primary_resolved(
        &b,
        &terminal(2200, 0, 0, 0),
        "route",
        113,
        true,
        Some(&resolved),
    );
    assert!((t.snapshot().traditional_cost - before.traditional_cost - 0.0198).abs() < 1e-12);
}

#[test]
fn receipt_policy_upgrade_freezes_legacy_segment_and_starts_cold() {
    use carry::models::{Route, resolve_model};
    let current = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://chatgpt.com/backend-api/codex/responses", true),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let mut old_resolution = serde_json::to_value(&current).unwrap();
    old_resolution
        .as_object_mut()
        .unwrap()
        .remove("scenario_cache_policy");
    old_resolution["provenance"]
        .as_object_mut()
        .unwrap()
        .remove("scenario_cache");
    let old_resolution: ResolvedModel = serde_json::from_value(old_resolution).unwrap();
    let mut t = Trajectory::default();
    // A genuine equal-token rewrite keeps this a divergent-forecast control.
    t.projected(100, 100, true);
    t.primary_resolved(
        &body(),
        &terminal(2000, 1000, 0, 0),
        "route",
        100,
        true,
        Some(&old_resolution),
    );
    let mut old = state(&t);
    old["receipts"]
        .as_object_mut()
        .unwrap()
        .remove("scenario_cache_policy_version");
    old["receipts"]
        .as_object_mut()
        .unwrap()
        .remove("traditional_anchor");
    old["result"]
        .as_object_mut()
        .unwrap()
        .remove("legacy_cache_policy_segment");
    old["result"]
        .as_object_mut()
        .unwrap()
        .remove("counterfactual_cache_policy");
    let mut t: Trajectory = serde_json::from_value(old).unwrap();
    let before = t.snapshot();
    assert_eq!(before.counterfactual_cache_policy, "legacy_route_cache_v0");
    let upgraded = old_resolution.with_scenario_cache_policy();
    assert_eq!(
        serde_json::to_value(upgraded.rates).unwrap(),
        serde_json::to_value(current.rates).unwrap()
    );
    assert!(upgraded.cache.is_none());
    let mut b = body();
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"new policy"}));
    t.primary_resolved(
        &b,
        &terminal(2100, 0, 0, 0),
        "route",
        101,
        true,
        Some(&upgraded),
    );
    let mixed = t.snapshot();
    assert_eq!(
        mixed.counterfactual_cache_policy,
        "mixed_versioned_cache_policies"
    );
    let segment = mixed.legacy_cache_policy_segment.as_ref().unwrap();
    assert_eq!(segment.traditional_cost, before.traditional_cost);
    assert_eq!(segment.carry_cost, before.carry_cost);
    assert_eq!(
        segment.matched_primary_responses,
        before.matched_primary_responses
    );
    let cold = scenario_input_cost("gpt-6.1-sol", 2100, 0, Some(1024)).unwrap();
    assert!((mixed.traditional_cost - before.traditional_cost - cold).abs() < 1e-12);
    let mut t: Trajectory = serde_json::from_value(state(&t)).unwrap();
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"warm"}));
    t.primary_resolved(
        &b,
        &terminal(2200, 0, 0, 0),
        "route",
        102,
        true,
        Some(&upgraded),
    );
    let after = t.snapshot();
    let warm = scenario_input_cost("gpt-6.1-sol", 2200, 2100, Some(1024)).unwrap();
    assert!((after.traditional_cost - mixed.traditional_cost - warm).abs() < 1e-12);
    assert_eq!(
        serde_json::to_value(after.legacy_cache_policy_segment).unwrap(),
        serde_json::to_value(mixed.legacy_cache_policy_segment).unwrap()
    );
    assert_eq!(after.coverage.primary_responses, 3);
    assert_eq!(after.matched_primary_responses, 3);
}

#[test]
fn receipt_traditional_cache_invalidations_are_independent_and_conservative() {
    use carry::models::{Route, resolve_model};
    for variant in [
        "instructions",
        "tools",
        "cache_key",
        "route",
        "auth",
        "tier",
        "model",
        "fork",
        "unknown_call",
        "backward_time",
        "edit",
    ] {
        let mut resolved = resolve_model(
            "gpt-6.1-sol",
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            0,
            0,
        )
        .unwrap();
        let mut t = Trajectory::default();
        // A genuine equal-token rewrite keeps this a divergent-forecast control.
        t.projected(100, 100, true);
        t.primary_resolved(
            &body(),
            &terminal(2000, 0, 0, 0),
            "route",
            100,
            true,
            Some(&resolved),
        );
        let before = t.snapshot();
        let mut b = body();
        b["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"append"}));
        let mut route = "route";
        let mut at = 101;
        match variant {
            "instructions" => b["instructions"] = json!("new"),
            "tools" => b["tools"] = json!([{"type":"function","name":"new"}]),
            "cache_key" => b["prompt_cache_key"] = json!("new"),
            "route" => route = "other",
            "auth" => resolved.route = Route::new("https://api.openai.com/v1/responses", true),
            "tier" => resolved.route.processing_tier = "priority".into(),
            "model" => {
                b["model"] = json!("gpt-6-luna");
                resolved =
                    resolve_model("gpt-6-luna", resolved.route.clone(), None, None, 0, 0).unwrap();
            }
            "fork" => t.reset_epoch(),
            "unknown_call" => t.failed_call(),
            "backward_time" => at = 99,
            "edit" => b["input"][0]["content"] = json!("unannounced edit"),
            _ => unreachable!(),
        }
        t.primary_resolved(
            &b,
            &terminal(2100, 0, 0, 0),
            route,
            at,
            true,
            Some(&resolved),
        );
        let expected = scenario_input_cost_resolved(
            b["model"].as_str().unwrap(),
            2100,
            0,
            Some(1024),
            Some(&resolved),
        )
        .unwrap();
        assert!(
            (t.snapshot().traditional_cost - before.traditional_cost - expected).abs() < 1e-12,
            "{variant}: must be cold"
        );
    }
}

#[test]
fn receipt_traditional_summary_is_cold_then_rewarms_its_own_prefix() {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 4000,
            reserve: 100,
            keep_recent: 1500,
            summary: 100,
        },
        ..Trajectory::default()
    };
    let mut r = terminal(3800, 0, 0, 0);
    r["output"] = json!([{"type":"message", "role":"assistant", "content":[{"type":"output_text","text":"visible ".repeat(1000)}]}]);
    let mut b = body();
    t.primary(&b, &r, "route", 100, true);
    assert_eq!(t.snapshot().traditional_compactions, 1);
    let before = t.snapshot();
    // A real continuation includes the retained visible output in total input.
    // Keep that measured quantity, rather than an inconsistent nominal 3900.
    let next_input = t.history.retired + 1700 - t.history.summary;
    for (at, input, traditional_input, cached) in [
        (101, next_input, 1700, 0),
        (102, next_input + 100, 1800, 1700),
    ] {
        b["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"next"}));
        let prior = t.snapshot();
        t.primary(&b, &terminal(input, 0, 0, 0), "route", at, true);
        let expected =
            scenario_input_cost("gpt-6.1-sol", traditional_input, cached, Some(1024)).unwrap();
        assert!((t.snapshot().traditional_cost - prior.traditional_cost - expected).abs() < 1e-12);
        assert_eq!(
            t.snapshot().traditional_compactions,
            before.traditional_compactions
        );
    }
}

#[test]
fn receipt_traditional_summary_is_cold_once_then_warms_its_own_prefix() {
    let mut t = Trajectory {
        scenario: Scenario {
            context_window: 3000,
            reserve: 100,
            keep_recent: 1200,
            summary: 20,
        },
        ..Trajectory::default()
    };
    let mut b = body();
    // This test owns charged in-window summary/cache behavior, not unknown
    // first-window initialization. Preserve its paid-summary assertions.
    t.primary(&b, &terminal(2800, 0, 0, 0), "route", 99, true);
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"in-window growth"}));
    t.primary(&b, &terminal(3000, 3000, 0, 0), "route", 100, true);
    assert_eq!(t.snapshot().initial_compactions, 0);
    assert_eq!(t.snapshot().traditional_compactions, 1);
    let before = t.snapshot();
    let mut t: Trajectory = serde_json::from_value(state(&t)).unwrap();
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"after summary"}));
    t.primary(&b, &terminal(3100, 3000, 0, 0), "route", 101, true);
    let cold = t.snapshot();
    assert!(
        (cold.traditional_cost
            - before.traditional_cost
            - scenario_input_cost("gpt-6.1-sol", 1320, 0, Some(1024)).unwrap())
        .abs()
            < 1e-12
    );
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"warm summary"}));
    t.primary(&b, &terminal(3200, 0, 0, 0), "route", 102, true);
    assert!(
        (t.snapshot().traditional_cost
            - cold.traditional_cost
            - scenario_input_cost("gpt-6.1-sol", 1420, 1320, Some(1024)).unwrap())
        .abs()
            < 1e-12
    );
    assert_eq!(t.snapshot().traditional_compactions, 1);
}

#[test]
fn receipt_unknown_scenario_cache_policy_stays_uncached_and_unknown_rates_stay_unpriced() {
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
    resolved
        .scenario_cache_policy
        .as_mut()
        .unwrap()
        .minimum_prefix_tokens = None;
    let mut t = Trajectory::default();
    // A genuine equal-token rewrite keeps this a divergent-forecast control.
    t.projected(100, 100, true);
    let mut b = body();
    for at in 100..102 {
        b["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"append"}));
        t.primary_resolved(
            &b,
            &terminal(2000, 2000, 0, 0),
            "route",
            at,
            true,
            Some(&resolved),
        );
    }
    let expected = scenario_input_cost("gpt-6.1-sol", 2000, 0, None).unwrap();
    assert!((t.snapshot().traditional_cost - 2.0 * expected).abs() < 1e-12);
    assert_eq!(t.snapshot().coverage.unknown_cache_requests, 2);
    let unknown = resolve_model(
        "not-a-known-model",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        Some(10000),
        0,
        0,
    )
    .unwrap();
    assert!(
        unknown
            .scenario_cache_policy
            .as_ref()
            .unwrap()
            .minimum_prefix_tokens
            .is_none()
    );
    assert!(unknown.cache.is_none());
    b["model"] = json!("not-a-known-model");
    let mut t = Trajectory::default();
    t.primary_resolved(
        &b,
        &terminal(2000, 1000, 0, 0),
        "route",
        100,
        true,
        Some(&unknown),
    );
    let public = serde_json::to_value(t.snapshot().dashboard(true)).unwrap();
    assert!(public["carry_cost"].is_null());
    assert!(public["observed_primary_cost"].is_null());
    assert_eq!(public["coverage"]["unknown_rate_calls"], 1);
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
            (primary.traditional_cost - expected).abs() < 1e-12,
            "identical execution shares native money, not imagined cache partitions"
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
            "memory" => t.projected(10, 20, true),
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

fn receipt_many_appends() -> Trajectory {
    let mut t = Trajectory::default();
    let mut b = body();
    for n in 0..32 {
        b["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"private-canary ".repeat(1000)}));
        t.primary(&b, &terminal(2000 + n, 0, 0, 0), "route", 100 + n, true);
    }
    t
}

#[test]
fn receipt_bounded_anchor_restart_and_legacy_window_do_not_reprice_history() {
    let t = receipt_many_appends();
    let receipt = state(&t)["receipts"].clone();
    let events = &receipt["modeled_cache"]["writes"];
    assert!(events.as_array().unwrap().len() <= 128);
    assert!(serde_json::to_vec(events).unwrap().len() < 8192);
    assert!(
        serde_json::to_vec(&receipt).unwrap().len() < 16384,
        "bounded numeric anchors and modeled extents, no prompt archive"
    );
    assert!(!receipt.to_string().contains("private-canary"));
    let resumed: Trajectory = serde_json::from_value(state(&t)).unwrap();
    assert_eq!(state(&resumed)["receipts"], receipt);
    assert_eq!(resumed.snapshot().carry_cost, t.snapshot().carry_cost);
    assert_eq!(
        resumed.snapshot().traditional_cost,
        t.snapshot().traditional_cost
    );
}

#[test]
fn receipt_legacy_window_does_not_reprice_history() {
    // Separate registration prevents a storage-shape failure from hiding these
    // monetary/mixed-legacy controls. The original lifecycle is retained.
    let t = receipt_many_appends();
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
