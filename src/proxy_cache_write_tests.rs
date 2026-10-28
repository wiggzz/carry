// Issue188: paid reviewer expense must not be masked by a synthetic write.
#[test]
fn cache_write188_reviewer_overhead_remains_a_loss_on_identical_no_write_input() {
    let mut t = Trajectory::default();
    let request = body();
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
