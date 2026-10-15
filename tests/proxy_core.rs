use std::collections::HashSet;

use carry::core::{
    Rates, UsageLedger, ViewCosts, horizon_cost, input_cost, joint_decision, prefix_compatible,
    select_removals,
};
use serde_json::json;

#[test]
fn joint_cost_vetoes_a_primary_only_positive_rewrite() {
    let primary = ViewCosts {
        keep: 100.0,
        compact: 90.0,
    };
    let shadow = ViewCosts {
        keep: 1.0,
        compact: 20.0,
    };
    assert!(primary.keep > primary.compact);
    let decision = joint_decision(primary, shadow, 0);
    assert!(!decision.accepted);
    assert_eq!(decision.savings, -9.0);
}

#[test]
fn joint_cost_admits_a_rewrite_repaid_by_future_shadow_savings() {
    let primary = ViewCosts {
        keep: 100.0,
        compact: 105.0,
    };
    let shadow = ViewCosts {
        keep: 40.0,
        compact: 10.0,
    };
    let decision = joint_decision(primary, shadow, 0);
    assert!(decision.accepted);
    assert_eq!(decision.savings, 25.0);
    assert!(!joint_decision(primary, shadow, 25).accepted);
}

#[test]
fn no_future_review_means_zero_shadow_cost_not_current_sunk_review() {
    assert_eq!(horizon_cost(1000.0, 50.0, 1.0, 0), 0.0);
    let decision = joint_decision(
        ViewCosts {
            keep: 10.0,
            compact: 8.0,
        },
        ViewCosts {
            keep: 0.0,
            compact: 0.0,
        },
        0,
    );
    assert!(decision.accepted);
    assert_eq!(decision.savings, 2.0);
}

#[test]
fn exact_prefix_append_preserves_reuse_but_rewrite_invalidates() {
    let original = vec![json!({"opaque": "a"}), json!({"opaque": "b"})];
    let mut appended = original.clone();
    appended.push(json!({"opaque": "c"}));
    assert!(prefix_compatible(&appended, &original));
    appended.remove(0);
    assert!(!prefix_compatible(&appended, &original));
}

#[test]
fn atomic_selection_preserves_order_and_external_protection() {
    let eligible = HashSet::from([7, 8, 9]);
    let protected = HashSet::from([8]);
    assert_eq!(select_removals([9, 1, 8, 7], &eligible, &protected), [9, 7]);
}

#[test]
fn exact_model_rates_and_full_request_long_context_threshold() {
    let luna = Rates::for_model("gpt-6-luna", 272000.0).unwrap();
    let long = Rates::for_model("gpt-6-luna", 272001.0).unwrap();
    assert_eq!(long.input, luna.input * 2.0);
    assert_eq!(long.cached, luna.cached * 2.0);
    assert_eq!(long.write, luna.write * 2.0);
    assert_eq!(long.output, luna.output * 1.5);
    let sol = Rates::for_model("gpt-6-sol", 1.0).unwrap();
    let newer = Rates::for_model("gpt-6.1-sol", 1.0).unwrap();
    assert_eq!(sol.input, newer.input);
    assert_eq!(sol.cached, newer.cached * 2.0);
    assert!(Rates::for_model("gpt-6-luna-alias", 1.0).is_none());
}

#[test]
fn native_usage_keeps_partitions_and_reasoning_is_not_extra_output() {
    let mut ledger = UsageLedger::default();
    ledger.observe(
        "gpt-6-luna",
        &json!({
            "input_tokens": 1000000, "output_tokens": 1000000,
            "input_tokens_details": {"cached_tokens": 100000, "cache_write_tokens": 100000},
            "output_tokens_details": {"reasoning_tokens": 900000}
        }),
        true,
    );
    assert_eq!(ledger.input_tokens, 1000000);
    assert_eq!(ledger.output_tokens, 1000000);
    assert_eq!(ledger.cached_tokens, 100000);
    assert_eq!(ledger.cache_write_tokens, 100000);
    assert!((ledger.cost_usd - 0.937).abs() < 0.000001);
    ledger.observe(
        "unknown",
        &json!({"input_tokens": 1, "output_tokens": 1}),
        true,
    );
    assert_eq!(ledger.unavailable_cost_calls, 1);
}

#[test]
fn missing_or_invalid_cache_partition_never_makes_a_cost_claim() {
    let mut ledger = UsageLedger::default();
    ledger.observe(
        "gpt-6-luna",
        &json!({"input_tokens": 100, "output_tokens": 1}),
        true,
    );
    ledger.observe("gpt-6-luna", &json!({"input_tokens": 10, "output_tokens": 1, "input_tokens_details": {"cached_tokens": 11}}), true);
    assert_eq!(ledger.unavailable_cost_calls, 2);
    assert_eq!(ledger.cost_usd, 0.0);
    assert_eq!(input_cost(100.0, 50.0, 2.0, 0.1), 105.0);
}

// These extreme receipts are structurally valid, not a realistic token volume.
// Exercise observe itself: arithmetic-only witnesses cannot catch partial writes.
fn assert_usage_aggregate_overflow_is_censored(first: [u64; 4], next: [u64; 4], known_cost: f64) {
    let receipt = |[input, output, cached, written]: [u64; 4]| {
        assert!(cached.checked_add(written).is_some_and(|sum| sum <= input));
        json!({
            "input_tokens": input,
            "output_tokens": output,
            "input_tokens_details": {
                "cached_tokens": cached,
                "cache_write_tokens": written
            }
        })
    };
    let counters = |ledger: &UsageLedger| {
        [
            ledger.input_tokens,
            ledger.output_tokens,
            ledger.cached_tokens,
            ledger.cache_write_tokens,
        ]
    };
    let mut ledger = UsageLedger::default();
    ledger.observe("gpt-6.1-sol", &receipt(first), true);
    assert_eq!(counters(&ledger), first);
    assert_eq!(ledger.calls, 1);
    assert_eq!(ledger.unavailable_cost_calls, 0);
    // An independent first-receipt rate-card calculation anchors the lower bound.
    assert!(known_cost.is_finite() && known_cost > 0.0);
    assert!((ledger.cost_usd - known_cost).abs() <= known_cost * 1e-12);
    let before = ledger.clone();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ledger.observe("gpt-6.1-sol", &receipt(next), true);
    }));
    assert!(result.is_ok(), "valid usage aggregation must not panic");
    let saturated: [u64; 4] = std::array::from_fn(|i| first[i].saturating_add(next[i]));
    // Either reject the whole receipt's token update or saturate every counter.
    // Do not require a new field or choose the production overflow policy here.
    assert!(
        counters(&ledger) == first || counters(&ledger) == saturated,
        "overflow must not wrap or leave only part of the token receipt applied: {:?}",
        counters(&ledger)
    );
    assert_eq!(ledger.calls, 2);
    assert!(
        ledger.unavailable_cost_calls > 0,
        "overflow must censor complete coverage using the existing public marker"
    );
    assert!(ledger.cost_usd.is_finite());
    assert!(ledger.cost_usd >= before.cost_usd);
    assert!(ledger.cost_usd >= known_cost - known_cost * 1e-12);

    let after_overflow = counters(&ledger);
    ledger.observe("gpt-6.1-sol", &receipt([0; 4]), true);
    assert_eq!(ledger.calls, 3);
    assert_eq!(counters(&ledger), after_overflow);
    assert!(ledger.unavailable_cost_calls > 0);
    assert!(ledger.cost_usd >= before.cost_usd);
}

#[test]
fn usage_aggregate_input_overflow_censors_without_partial_mutation() {
    assert_usage_aggregate_overflow_is_censored(
        [u64::MAX, 7, 0, 0],
        [1, 11, 0, 0],
        u64::MAX as f64 * 4.0 / 1_000_000.0 + 7.0 * 15.0 / 1_000_000.0,
    );
}

#[test]
fn usage_aggregate_output_overflow_censors_without_partial_mutation() {
    assert_usage_aggregate_overflow_is_censored(
        [9, u64::MAX, 2, 3],
        [1, 1, 0, 1],
        (4.0 * 2.0 + 2.0 * 0.1 + 3.0 * 2.5 + u64::MAX as f64 * 10.0) / 1_000_000.0,
    );
}

#[test]
fn usage_aggregate_cached_overflow_censors_without_partial_mutation() {
    assert_usage_aggregate_overflow_is_censored(
        [u64::MAX, 7, u64::MAX, 0],
        [1, 11, 1, 0],
        u64::MAX as f64 * 0.2 / 1_000_000.0 + 7.0 * 15.0 / 1_000_000.0,
    );
}

#[test]
fn usage_aggregate_cache_write_overflow_censors_without_partial_mutation() {
    assert_usage_aggregate_overflow_is_censored(
        [u64::MAX, 7, 0, u64::MAX],
        [1, 11, 0, 1],
        u64::MAX as f64 * 5.0 / 1_000_000.0 + 7.0 * 15.0 / 1_000_000.0,
    );
}

#[test]
fn unsupported_billed_input_categories_make_native_cost_unavailable() {
    let mut ledger = UsageLedger::default();
    ledger.observe(
        "gpt-6-luna",
        &json!({
            "input_tokens": 100, "output_tokens": 2,
            "input_tokens_details": {"cached_tokens": 0, "audio_tokens": 5}
        }),
        true,
    );
    assert_eq!(ledger.unavailable_cost_calls, 1);
    assert_eq!(ledger.cost_usd, 0.0);
}

#[test]
fn unknown_model_receipts_explain_unpriced_calls_without_guessing_rates() {
    let mut ledger = carry::core::UsageLedger::default();
    ledger.observe(
        "codex-auto-review",
        &serde_json::json!({
            "input_tokens": 100, "output_tokens": 10,
            "input_tokens_details": {"cached_tokens": 0}
        }),
        true,
    );
    assert_eq!(ledger.unavailable_cost_calls, 1);
    assert_eq!(ledger.cost_usd, 0.0);
    let value = serde_json::to_value(&ledger).unwrap();
    assert_eq!(
        value["unpriced_models"],
        serde_json::json!(["codex-auto-review"])
    );
}
