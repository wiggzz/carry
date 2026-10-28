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
