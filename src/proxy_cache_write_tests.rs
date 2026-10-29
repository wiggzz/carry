// Issue188: explicit-only with no breakpoints must not mask reviewer expense
// with a synthetic write. Default/implicit paid-write controls stay separate.
#[test]
fn cache_write188_reviewer_overhead_remains_a_loss_on_identical_no_write_input() {
    let mut t = Trajectory::default();
    let mut request = body();
    request["prompt_cache_options"] = json!({"mode":"explicit", "ttl":"30m"});
    let response = terminal(43_298, 0, 0, 100);
    t.primary(&request, &response, "no-write-fixture", 100, true);
    t.reviewer(&request, &terminal(1000, 0, 0, 100), true);
    let estimate = t.snapshot();
    assert!(estimate.priced);
    assert_eq!(estimate.matched_primary_responses, 1);
    assert_eq!(estimate.coverage.reviewer_responses, 1);
    assert_eq!(estimate.traditional_compactions, 0);
    let primary = 43_298.0 * 2.0 / 1e6 + 100.0 * 10.0 / 1e6;
    let reviewer = 1000.0 * 2.0 / 1e6 + 100.0 * 10.0 / 1e6;
    assert!((estimate.observed_primary_cost - primary).abs() < 1e-12);
    assert!((estimate.observed_reviewer_cost - reviewer).abs() < 1e-12);
    assert!((estimate.reviewer_cost - reviewer).abs() < 1e-12);
    assert!((estimate.carry_cost - primary - reviewer).abs() < 1e-12);
    assert!(
        (estimate.savings + reviewer).abs() < 1e-12,
        "CACHE_WRITE_188_REVIEW: paid reviewer expense must remain a loss without a context benefit"
    );
}

// Acceptance coverage for existing paid-write accounting, not a new defect.
#[test]
fn cache_write188_paid_implicit_receipts_and_append_reads_survive_restart() {
    let mut t = Trajectory::default();
    let mut request = body();
    request["prompt_cache_key"] = json!("cache-write-188");
    request["prompt_cache_options"] = json!({"mode":"implicit", "ttl":"30m"});
    let mut expected = 0.0;
    for (index, (input, cached, written)) in
        [(2000, 0, 2000), (2100, 2000, 100)].into_iter().enumerate()
    {
        if index > 0 {
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user", "content":"append"}));
        }
        t.primary(
            &request,
            &terminal(input, cached, written, 0),
            "paid-implicit-fixture",
            100 + index as u64,
            true,
        );
        expected += (cached as f64 * 0.10 + written as f64 * 2.50) / 1e6;
        let snapshot = t.snapshot();
        assert!(snapshot.priced);
        assert_eq!(snapshot.matched_primary_responses, index as u64 + 1);
        assert_eq!(snapshot.traditional_compactions, 0);
        assert_eq!(snapshot.summary_cost, 0.0);
        assert_eq!(snapshot.reviewer_cost, 0.0);
        assert!((snapshot.observed_primary_cost - expected).abs() < 1e-12);
        assert!((snapshot.carry_cost - expected).abs() < 1e-12);
        assert!((snapshot.traditional_cost - expected).abs() < 1e-12);
        assert!(snapshot.savings.abs() < 1e-12);
        t = serde_json::from_slice(&serde_json::to_vec(&t).unwrap()).unwrap();
    }
}

// Additional acceptance coverage for labeling and later legacy transitions.
// These bodies are not claimed as separately observed failing-first tests.
#[test]
fn cache_policy188_version_labels_and_first_legacy_segment_survive_later_transitions() {
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
    let mut trajectory = Trajectory::default();
    trajectory
        .receipts
        .as_mut()
        .unwrap()
        .scenario_cache_policy_version = Some(7);
    assert_eq!(
        trajectory.snapshot().counterfactual_cache_policy,
        "conditional_model_cache_v7"
    );
    trajectory
        .receipts
        .as_mut()
        .unwrap()
        .scenario_cache_policy_version = None;
    let mut request = body();
    let mut original_legacy = Value::Null;
    let mut prior_boundaries = json!([]);
    for (index, version) in [None, Some(1), Some(2), None, Some(3), Some(1)]
        .into_iter()
        .enumerate()
    {
        let mut policy = resolved.clone();
        if let Some(version) = version {
            policy.scenario_cache_policy.as_mut().unwrap().version = version;
        } else {
            policy.scenario_cache_policy = None;
        }
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user", "content":format!("transition {index}")}));
        let before = trajectory.snapshot();
        trajectory.primary_resolved(
            &request,
            &terminal(2000 + index as u64 * 100, 0, 0, 0),
            "policy-fixture",
            100 + index as u64,
            true,
            Some(&policy),
        );
        let after = serde_json::to_value(trajectory.snapshot()).unwrap();
        if index == 0 {
            assert_eq!(
                after["counterfactual_cache_policy"],
                "legacy_route_cache_v0"
            );
            assert!(after.get("legacy_cache_policy_segment").is_none());
        } else {
            let boundaries = after["cache_policy_boundaries"].as_array().unwrap();
            assert_eq!(boundaries.len(), index);
            assert_eq!(
                &boundaries[..index - 1],
                prior_boundaries.as_array().unwrap()
            );
            let cumulative = serde_json::to_value(CachePolicySegment::from(&before)).unwrap();
            assert_eq!(boundaries[index - 1]["cumulative"], cumulative);
            if index == 1 {
                original_legacy = after["legacy_cache_policy_segment"].clone();
                assert_eq!(original_legacy, cumulative);
                assert_eq!(
                    after["counterfactual_cache_policy"],
                    "mixed_legacy_route_v0_and_model_v1"
                );
            } else {
                assert_eq!(after["legacy_cache_policy_segment"], original_legacy);
                assert_eq!(
                    after["counterfactual_cache_policy"],
                    "mixed_versioned_cache_policies"
                );
            }
            prior_boundaries = json!(boundaries);
        }
        trajectory = serde_json::from_slice(&serde_json::to_vec(&trajectory).unwrap()).unwrap();
    }
}

// Placeholder future version numbers exercise the migration owner, not an
// assertion that either future write-billing policy has been implemented.
#[test]
fn cache_policy188_present_version_boundaries_preserve_primary_and_reviewer_money() {
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
    let mut trajectory = Trajectory::default();
    // Seed both the synthetic owner and dispatch contract before paid coverage.
    resolved.scenario_cache_policy.as_mut().unwrap().version = 1;
    trajectory
        .receipts
        .as_mut()
        .unwrap()
        .scenario_cache_policy_version = Some(1);
    let mut request = body();
    trajectory.reviewer_resolved(&request, &terminal(1000, 0, 0, 100), true, Some(&resolved));
    let mut snapshots = Vec::new();
    for (index, version) in [1, 2, 2, 3].into_iter().enumerate() {
        resolved.scenario_cache_policy.as_mut().unwrap().version = version;
        if index > 0 {
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user", "content":format!("append {index}")}));
        }
        let input = 2000 + index as u64 * 100;
        let cached = if index == 0 { 0 } else { input - 100 };
        trajectory.primary_resolved(
            &request,
            &terminal(input, cached, 0, 0),
            "policy-fixture",
            100 + index as u64,
            true,
            Some(&resolved),
        );
        let snapshot = trajectory.snapshot();
        assert!(snapshot.priced);
        assert_eq!(snapshot.matched_primary_responses, index as u64 + 1);
        assert_eq!(snapshot.coverage.reviewer_responses, 1);
        assert!((snapshot.observed_reviewer_cost - 0.003).abs() < 1e-12);
        let actual = 0.004
            + index as f64 * 0.0002
            + (0..index)
                .map(|n| (2000 + n * 100) as f64 * 0.1 / 1e6)
                .sum::<f64>();
        assert!((snapshot.observed_primary_cost - actual).abs() < 1e-12);
        assert!((snapshot.carry_cost - actual - 0.003).abs() < 1e-12);
        snapshots.push(serde_json::to_value(snapshot).unwrap());
        // Exercise the production checkpoint codec between every observation.
        trajectory = serde_json::from_slice(&serde_json::to_vec(&trajectory).unwrap()).unwrap();
    }
    let boundaries = snapshots[1]["cache_policy_boundaries"].as_array();
    assert!(
        boundaries.is_some_and(|b| b.len() == 1),
        "CACHE_POLICY_188_NATIVE: present v1 -> present v2 must freeze cumulative provenance"
    );
    let boundaries = boundaries.unwrap();
    assert_eq!(boundaries[0]["previous_version"], 1);
    assert_eq!(boundaries[0]["next_version"], 2);
    for field in [
        "carry_cost",
        "traditional_cost",
        "reviewer_cost",
        "summary_cost",
        "traditional_compactions",
        "coverage",
        "matched_primary_responses",
    ] {
        assert_eq!(boundaries[0]["cumulative"][field], snapshots[0][field]);
    }
    assert_eq!(snapshots[2]["cache_policy_boundaries"], json!(boundaries));
    let later = snapshots[3]["cache_policy_boundaries"].as_array().unwrap();
    assert_eq!(later.len(), 2);
    assert_eq!(later[0], boundaries[0]);
    assert_eq!(later[1]["previous_version"], 2);
    assert_eq!(later[1]["next_version"], 3);
    assert_eq!(
        later[1]["cumulative"]["reviewer_cost"],
        snapshots[2]["reviewer_cost"]
    );
    assert_eq!(
        snapshots[3]["counterfactual_cache_policy"],
        "mixed_versioned_cache_policies"
    );
}

// Prospective v1 -> request-aware v2 upgrade: historical money is immutable.
#[test]
fn cache_write188_resolution_upgrade_keeps_old_money_and_disables_future_reads_and_writes() {
    use carry::models::{Route, resolve_model};
    let mut old = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    old.scenario_cache_policy.as_mut().unwrap().version = 1;
    old.scenario_cache_policy
        .as_mut()
        .unwrap()
        .minimum_prefix_tokens = Some(1500);
    old.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 7;
    let mut request = body();
    request["prompt_cache_options"] = json!({"mode":"explicit", "ttl":"30m"});
    let mut t = Trajectory::default();
    t.primary_resolved(
        &request,
        &terminal(2000, 0, 0, 0),
        "fixture",
        100,
        true,
        Some(&old),
    );
    let before = t.snapshot();
    assert!((before.observed_primary_cost - 0.004).abs() < 1e-12);
    assert!((before.traditional_cost - 0.005).abs() < 1e-12);
    let upgraded = old.clone().with_scenario_cache_policy();
    assert_eq!(upgraded.scenario_cache_policy.as_ref().unwrap().version, 2);
    assert_eq!(
        upgraded
            .scenario_cache_policy
            .as_ref()
            .unwrap()
            .minimum_prefix_tokens,
        Some(1500)
    );
    assert_eq!(
        upgraded.scenario_cache_policy.as_ref().unwrap().ttl_seconds,
        7
    );
    assert_eq!(
        serde_json::to_value(upgraded.rates).unwrap(),
        serde_json::to_value(old.rates).unwrap()
    );
    for (index, input) in [2100, 2200].into_iter().enumerate() {
        t = serde_json::from_slice(&serde_json::to_vec(&t).unwrap()).unwrap();
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user", "content":format!("append {index}")}));
        t.primary_resolved(
            &request,
            &terminal(input, 0, 0, 0),
            "fixture",
            101 + index as u64,
            true,
            Some(&upgraded),
        );
        let after = t.snapshot();
        let prospective = if index == 0 { 0.0042 } else { 0.0086 };
        assert!((after.traditional_cost - before.traditional_cost - prospective).abs() < 1e-12);
        assert!(
            (after.observed_primary_cost - before.observed_primary_cost - prospective).abs()
                < 1e-12
        );
        assert_eq!(after.cache_policy_boundaries.len(), 1);
        assert_eq!(after.cache_policy_boundaries[0].previous_version, Some(1));
        assert_eq!(after.cache_policy_boundaries[0].next_version, Some(2));
        assert_eq!(
            serde_json::to_value(&after.cache_policy_boundaries[0].cumulative).unwrap(),
            serde_json::to_value(CachePolicySegment::from(&before)).unwrap()
        );
        assert_eq!(
            after.counterfactual_cache_policy,
            "mixed_versioned_cache_policies"
        );
    }
}

// Acceptance: compare absolute prices across branch histories, cache modes,
// native receipt tier boundaries and checkpoint restarts (not cancellation).
#[test]
fn cache_write191_divergent_history_uses_frozen_branch_tiers_and_independent_cache() {
    use carry::models::{Route, resolve_model};
    for mode in ["implicit", "explicit"] {
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
                context_window: 1_000_000,
                ..Scenario::default()
            },
            ..Trajectory::default()
        };
        let mut request = body();
        request["prompt_cache_options"] = json!({"mode":mode,"ttl":"30m"});
        request["prompt_cache_key"] = json!("divergent");
        let mut traditional = 0.0;
        let mut actual = 0.0;
        for index in 0..3 {
            let input = 200_000 + index * 100;
            if index > 0 {
                request["input"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"role":"user","content":"append"}));
                if index == 1 {
                    t.projected(100_100, 100, true);
                }
            }
            // Actual cache stays cold: its receipt must not set the alternative's hit.
            t.primary_resolved(
                &request,
                &terminal(input, 0, 0, 10_000),
                "fixture",
                100 + index,
                true,
                Some(&resolved),
            );
            actual += (input as f64 * 2.0 + 100_000.0) / 1e6;
            let alternative = input + if index > 0 { 100_000 } else { 0 };
            let cached = if mode == "implicit" && index > 0 {
                if index == 1 { 200_000 } else { 300_100 }
            } else {
                0
            };
            let (ordinary, read, write, output) = if alternative > 272_000 {
                (4.0, 0.2, 5.0, 15.0)
            } else {
                (2.0, 0.1, 2.5, 10.0)
            };
            traditional += (if mode == "implicit" {
                cached as f64 * read + (alternative - cached) as f64 * write
            } else {
                alternative as f64 * ordinary
            } + 10_000.0 * output)
                / 1e6;
            let estimate = t.snapshot();
            assert!(estimate.priced);
            assert_eq!(estimate.matched_primary_responses, index as usize + 1);
            assert_eq!(estimate.traditional_compactions, 0);
            assert!((estimate.observed_primary_cost - actual).abs() < 1e-12);
            assert!((estimate.carry_cost - actual).abs() < 1e-12);
            assert!(
                (estimate.traditional_cost - traditional).abs() < 1e-12,
                "{mode} branch-specific cache/tier"
            );
            assert!((estimate.savings - (traditional - actual)).abs() < 1e-12);
            t = serde_json::from_slice(&serde_json::to_vec(&t).unwrap()).unwrap();
        }
    }
}

#[test]
fn cache_write191_post_summary_cold_then_warm_obeys_mode_after_restart() {
    for mode in ["implicit", "explicit"] {
        let mut t = Trajectory {
            scenario: Scenario {
                context_window: 3000,
                reserve: 100,
                keep_recent: 1200,
                summary: 20,
            },
            ..Trajectory::default()
        };
        let mut request = body();
        request["prompt_cache_options"] = json!({"mode":mode,"ttl":"30m"});
        t.primary(&request, &terminal(3000, 3000, 0, 0), "fixture", 100, true);
        let first = t.snapshot();
        assert!(first.priced);
        assert_eq!(first.traditional_compactions, 1);
        // Summary request is ordinary uncached at its own input tier.
        assert!((first.summary_cost - (1800.0 * 2.0 + 20.0 * 10.0) / 1e6).abs() < 1e-12);
        let mut expected = first.traditional_cost;
        for (index, input) in [3100, 3200].into_iter().enumerate() {
            t = serde_json::from_slice(&serde_json::to_vec(&t).unwrap()).unwrap();
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":"continue"}));
            t.primary(
                &request,
                &terminal(input, 3000, 0, 0),
                "fixture",
                101 + index as u64,
                true,
            );
            let alternative = input - 1780;
            let cached = if mode == "implicit" && index > 0 {
                1320
            } else {
                0
            };
            expected += if mode == "implicit" {
                (cached as f64 * 0.1 + (alternative - cached) as f64 * 2.5) / 1e6
            } else {
                alternative as f64 * 2.0 / 1e6
            };
            let after = t.snapshot();
            assert!(after.priced);
            assert_eq!(after.traditional_compactions, 1);
            assert_eq!(after.summary_cost, first.summary_cost);
            assert!(
                (after.traditional_cost - expected).abs() < 1e-12,
                "{mode} post-summary price"
            );
        }
    }
}

// Unknown request contracts exclude comparison, never actual paid receipts.
#[test]
fn cache_write191_unsupported_lifetime_preserves_known_money_in_both_receipt_lanes() {
    for reviewer in [false, true] {
        let mut t = Trajectory::default();
        let mut request = body();
        request["prompt_cache_options"] = json!({"mode":"implicit","ttl":"30m"});
        let mut expected = 0.0;
        for (index, (ttl, written)) in [("30m", 2000), ("12h", 2000), ("12h", 0), ("30m", 2000)]
            .into_iter()
            .enumerate()
        {
            request["prompt_cache_options"]["ttl"] = json!(ttl);
            if reviewer {
                t.reviewer(&request, &terminal(2000, 0, written, 10), true);
            } else {
                t.primary(
                    &request,
                    &terminal(2000, 0, written, 10),
                    "fixture",
                    100 + index as u64,
                    true,
                );
            }
            if ttl == "30m" || written == 0 {
                expected += (2000.0 * if written == 0 { 2.0 } else { 2.5 } + 100.0) / 1e6;
            }
            let snapshot = t.snapshot();
            let (known, unavailable) = if reviewer {
                (
                    snapshot.observed_reviewer_cost,
                    snapshot.unavailable_reviewer_cost_calls,
                )
            } else {
                (
                    snapshot.observed_primary_cost,
                    snapshot.unavailable_primary_cost_calls,
                )
            };
            assert!(
                (known - expected).abs() < 1e-12,
                "CACHE_WRITE_191_TTL: unsupported writes must not acquire the 30m price"
            );
            assert_eq!(unavailable, u64::from(index > 0));
            t = serde_json::from_slice(&serde_json::to_vec(&t).unwrap()).unwrap();
        }
    }
}

#[test]
fn cache_write188_unknown_mode_ttl_or_explicit_extent_keeps_actual_money() {
    for options in [
        json!({"mode":"unknown"}),
        json!({"mode":"implicit","ttl":"12h"}),
        json!({"mode":null}),
        json!(null),
        json!({"mode":"explicit"}),
    ] {
        let mut request = body();
        request["prompt_cache_options"] = options.clone();
        if options == json!({"mode":"explicit"}) {
            request["input"] = json!([{"role":"user","content":[{"type":"input_text","text":"prefix","prompt_cache_breakpoint":{"mode":"explicit"}}]}]);
        }
        let mut t = Trajectory::default();
        t.primary(&request, &terminal(2000, 0, 0, 0), "fixture", 100, true);
        let snapshot = t.snapshot();
        assert!((snapshot.observed_primary_cost - 0.004).abs() < 1e-12);
        assert_eq!(snapshot.unavailable_primary_cost_calls, 0);
        assert_eq!(snapshot.coverage.unknown_cache_requests, 1);
        assert_eq!(snapshot.excluded_unpriced_calls, 1);
        assert_eq!(snapshot.matched_primary_responses, 0);
        assert!(!snapshot.priced);
        let dashboard = serde_json::to_value(snapshot.dashboard(false)).unwrap();
        assert!(dashboard["traditional_cost"].is_null());
        assert!(dashboard["savings"].is_null());
        assert!((dashboard["observed_primary_cost"].as_f64().unwrap() - 0.004).abs() < 1e-12);
    }
}
