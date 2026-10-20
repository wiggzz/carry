// Matched direct-component diagnostics, not release or HTTP benchmarks.
fn lease_work_fixture(horizon: u64, extra: bool) -> (ProxyCli, Session, Value) {
    let mut config = reviewer_config();
    config.payoff_requests = horizon;
    config.min_payback_percent = 0;
    config.review_min_new_tokens = if extra { 25_000 } else { 0 };
    let mut session = lease_many_cohorts(12, 8);
    let groups = session.groups();
    let due = session.due_keep_targets();
    assert_eq!(due.len(), 4);
    let eligible = groups
        .iter()
        .filter(|g| !g.human && !due.contains_key(&format!("g{}", g.id)))
        .map(|g| format!("g{}", g.id))
        .collect::<Vec<_>>();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":eligible,"memories":[]}),
        )
        .unwrap();
    let groups = session.groups();
    session.observe_groups(&groups);
    admission::cover(&mut session, &groups);
    let body = json!({"model":"gpt-6-sol","input":session.render_primary()});
    for length in [4, 7, 10, 13, 16, 19, 22, 25, 28, 31] {
        let mut prefix = body.clone();
        prefix["input"].as_array_mut().unwrap().truncate(length);
        observe_cache(
            &mut session.primary_cache,
            &prefix,
            &json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":100}}),
        );
    }
    assert_eq!(session.primary_cache.len(), 8);
    (config, session, body)
}

#[test]
fn lease_performance_frozen_forecast_projects_primary_once() {
    use std::io::Write;
    let mut violations = Vec::new();
    for (name, horizon, extra) in [
        ("ordinary_default_payoff5", 5, false),
        ("ordinary_max_payoff100", 100, false),
        ("extra_admission_bounded_payoff5", 5, true),
    ] {
        let (config, session, body) = lease_work_fixture(horizon, extra);
        let before = serde_json::to_value(&session).unwrap();
        planner_work::start();
        let started = std::time::Instant::now();
        let (candidate, report) = plan(&config, &session, &body);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let counts = planner_work::finish();
        assert_eq!(serde_json::to_value(&session).unwrap(), before);
        let candidate = candidate.expect("same real legal economic candidate must survive");
        let actions = report["candidates"].as_array().unwrap();
        assert_eq!(actions.len(), 6);
        if extra {
            assert!(counts.get("economic_probes").copied().unwrap_or(0) > 0);
        }
        let selected_primary = candidate.render_primary();
        let selected_reviewer = review_body(&config, &candidate);
        let selected = json!({"primary":selected_primary,"reviewer":selected_reviewer,
            "memories":candidate.memories,"opinions":candidate.opinions,"leases":candidate.keep_leases});
        let output_sha256 = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&selected).unwrap())
        );
        let bound = counts["forecasts"] + counts["plans"];
        writeln!(
            std::io::stdout(),
            "LEASE_PLANNER_WORK {}",
            json!({
                "scenario":name,"horizon":horizon,"extra_admission":extra,
                "build":"debug","boundary":"direct_component_not_HTTP",
                "counts":counts,"projection_bound":bound,"elapsed_ms":elapsed_ms,
                "report":report,"selected_sha256":output_sha256,
                "claim":"matched scenario only; no worst-case or release/HTTP latency claim"
            })
        )
        .unwrap();
        if counts["admission_primary_projections"] > bound {
            violations.push((name, counts["admission_primary_projections"], bound));
        }
    }
    assert!(
        violations.is_empty(),
        "LEASE_INVARIANT_PROJECTION_RED: frozen history/removals/memories and wire policy require at most one actual primary render/tokenize per forecast plus each plan report: {violations:?}"
    );
}
