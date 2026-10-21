// Synthetic private planner seams; no live services, providers or paid usage.
use super::*;

fn output_config() -> ProxyCli {
    let mut config = ProxyCli::try_parse_from(["carry proxy", "--mode", "compact"]).unwrap();
    config.payoff_requests = 1;
    config.min_payback_percent = 0;
    config.primary_context_tokens = 1_000_000;
    config.context_margin_percent = 0;
    // Reviewer capacity is independent; no hypothetical reviewer debit/credit
    // should mask the primary output-tariff counterexample.
    config.classifier_max_input_tokens = 128;
    config
}

fn output_session(retained: usize, disposable: usize) -> Session {
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"durable ".repeat(retained)}),
            json!({"role":"assistant","content":"discard ".repeat(disposable)}),
            json!({"role":"user","content":"finish"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    let groups = session.groups();
    session.observe_groups(&groups);
    session
        .apply_advice(
            &groups,
            &json!({"protected":["g1"],"removable":["g2"],"memories":[]}),
        )
        .unwrap();
    let route = Route::new("https://api.openai.com/v1/responses", false);
    session.model_resolution = Some(ResolvedPair {
        schema_version: 1,
        primary: resolve_model("gpt-6-sol", route.clone(), None, None, 1_000_000, 16384, 0)
            .unwrap(),
        reviewer: resolve_model("gpt-6-luna", route, None, None, 300000, 16384, 0).unwrap(),
    });
    session
}

fn output_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
}

#[test]
fn output_allowance_actual_tiers_reverse_selection_without_repricing_input_or_receipts() {
    let config = output_config();
    let seed = output_session(200_000, 80_000);
    let mut compacted = seed.clone();
    compacted.remove(&[2], &seed.groups());
    let resolved = &seed.model_resolution.as_ref().unwrap().primary;
    let mut ids = Vec::new();
    for cap in [0u64, 100_000] {
        let mut session = seed.clone();
        let body =
            json!({"model":"gpt-6-sol","input":session.render_primary(),"max_output_tokens":cap});
        let mut compact = body.clone();
        compact["input"] = json!(compacted.render_primary());
        let kt = estimate(&primary_wire_body(&config, &body));
        let ct = estimate(&primary_wire_body(&config, &compact));
        assert!(
            kt > 272000.0 && ct < 272000.0,
            "actual rendered tiers: {kt}/{ct}"
        );
        observe_primary_cache(
            &mut session.primary_cache,
            &body,
            &json!({"input_tokens":300000,"input_tokens_details":{"cached_tokens":300000}}),
            Some(resolved),
        );
        let input = resolved_view_costs(
            &body,
            &compact,
            &session.primary_cache,
            "gpt-6-sol",
            1,
            Some(resolved),
        )
        .unwrap();
        assert!(
            input.compact > input.keep,
            "cold low tier loses to warm high tier on input alone"
        );
        let kr = resolved.rates_for_input(kt).unwrap();
        let cr = resolved.rates_for_input(ct).unwrap();
        assert!(kr.output > cr.output);
        let (selected, report) = plan(&config, &session, &body);
        let row = report["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["removed_groups"] == json!([2]))
            .unwrap();
        // Preserve legacy input-only recurring sensitivity fields exactly.
        assert!(
            row["joint"].is_null(),
            "deferred reviewer forecast remains unknown"
        );
        if cap == 0 {
            assert!(selected.is_none(), "zero cap preserves warm input winner");
        } else {
            assert!(
                selected.is_some(),
                "OUTPUT_ALLOWANCE_RED: each branch's input-selected output tariff must reverse the actual planner choice"
            );
            assert_eq!(
                selected.unwrap().render_primary(),
                compacted.render_primary()
            );
        }
        output_close(
            row["selection_joint"]["primary_keep"].as_f64().unwrap(),
            input.keep + cap as f64 * kr.output,
        );
        output_close(
            row["selection_joint"]["primary_compact"].as_f64().unwrap(),
            input.compact + cap as f64 * cr.output,
        );
        ids.push(row["candidate_id"].clone());
        assert_eq!(
            session.primary.calls, 0,
            "sensitivity must not become paid receipt accounting"
        );
    }
    assert!(ids[0].is_string());
    assert_eq!(
        ids[0], ids[1],
        "candidate identity is not a cost/cap/ranking index"
    );
}

#[test]
fn output_allowance_same_rate_cancels_and_missing_cap_is_explicit_zero_not_headroom() {
    let mut config = output_config();
    config.payoff_requests = 3;
    config.primary_output_reserve_tokens = 99999;
    let session = output_session(100, 80);
    let mut prior = None;
    for cap in [None, Some(0u64), Some(100_000)] {
        let mut body = json!({"model":"gpt-6-sol","input":session.render_primary()});
        if let Some(cap) = cap {
            body["max_output_tokens"] = json!(cap);
        }
        let (candidate, report) = plan(&config, &session, &body);
        assert!(candidate.is_some());
        let row = &report["candidates"][0];
        let mut compact = body.clone();
        compact["input"] = json!(candidate.unwrap().render_primary());
        let input = resolved_view_costs(
            &body,
            &compact,
            &[],
            "gpt-6-sol",
            3,
            session.model_resolution.as_ref().map(|p| &p.primary),
        )
        .unwrap();
        let allowance = cap.unwrap_or(0) as f64
            * 3.0
            * Rates::for_model("gpt-6-sol", estimate(&body))
                .unwrap()
                .output;
        let joint = &row["selection_joint"];
        output_close(
            joint["primary_keep"].as_f64().unwrap(),
            input.keep + allowance,
        );
        output_close(
            joint["primary_compact"].as_f64().unwrap(),
            input.compact + allowance,
        );
        output_close(
            joint["savings"].as_f64().unwrap(),
            input.keep - input.compact,
        );
        if let Some(id) = &prior {
            assert_eq!(&row["candidate_id"], id);
        }
        prior = Some(row["candidate_id"].clone());
        assert_eq!(report["primary_output_allowance_tokens"], cap.unwrap_or(0));
        assert_eq!(
            report["primary_output_allowance_basis"],
            if cap.is_some() {
                "request_max_output_tokens_cap_sensitivity_not_expected_generation"
            } else {
                "legacy_zero_no_explicit_request_cap_not_headroom"
            }
        );
    }
}

#[tokio::test]
async fn action_identity_forced_selection_and_dashboard_follow_actual_render_not_max_savings() {
    let mut config = output_config();
    let mut session = output_session(100, 800);
    let body = json!({"model":"gpt-6-sol","input":session.render_primary()});
    let mut expected = session.clone();
    expected.remove(&[2], &session.groups());
    let mut compact = body.clone();
    compact["input"] = json!(expected.render_primary());
    config.primary_context_tokens = primary_required_tokens(&config, &compact).ceil() as u32;
    assert!(!primary_fits(&config, &body) && primary_fits(&config, &compact));
    let (selected, report) = plan(&config, &session, &body);
    assert!(report["forced"].as_bool().unwrap());
    assert_eq!(
        selected.unwrap().render_primary(),
        expected.render_primary()
    );
    let id = report["selected_candidate_id"]
        .as_str()
        .expect("ACTION_IDENTITY_RED: selected rendering needs an explicit stable identity")
        .to_owned();
    let row = report["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["candidate_id"] == id)
        .unwrap()
        .clone();
    assert_eq!(row["action"], "joint");
    assert_eq!(row["removed_groups"], json!([2]));
    assert_eq!(report["selected_action"], "joint");
    assert_eq!(report["keep_candidate"]["action"], "keep");
    // Synthetic saved-report decoy: huge recurring savings on an infeasible,
    // unselected row must never masquerade as the actual forced action.
    session.last_plan = report;
    session.last_plan["candidates"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "candidate_id":"decoy-unselected", "action":"reviewer_only", "feasible":false,
            "joint":{"keep":999.0,"compact":0.0,"savings":999.0},
            "selection_joint":{"keep":888.0,"compact":0.0,"savings":888.0}
        }));
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(super::checkpoint_service(directory.path()));
    save(&service, &"a".repeat(64), &session).unwrap();
    let response = dashboard_stats(
        State(service),
        ConnectInfo("127.0.0.1:1234".parse().unwrap()),
        HeaderMap::new(),
    )
    .await
    .unwrap();
    let stats: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let planner = &stats["sessions"][0]["planner"];
    assert_eq!(planner["selected_candidate_id"], id);
    assert_eq!(planner["selected_action"], "joint");
    assert_eq!(planner["candidate"]["candidate_id"], id);
    assert_eq!(
        planner["candidate"]["savings"],
        row["selection_joint"]["savings"]
    );
}

#[test]
fn action_identity_reviewer_only_preserves_primary_and_keep_has_no_fake_primary_only_action() {
    let mut config = output_config();
    config.classifier_max_input_tokens = 300000;
    config.review_min_primary_tokens = 0;
    config.review_min_new_tokens = 0;
    config.review_every_requests = 1;
    let mut session = output_session(100, 80);
    session.opinions.clear();
    session.drop_members.clear();
    session.completed_requests = 1;
    let body =
        json!({"model":"gpt-6-sol","input":session.render_primary(),"max_output_tokens":500});
    // Accepted source authority remains separate from disposable native turns.
    session.review_history = Some(crate::proxy_state::ReviewHistory {
        source_len: 0,
        source_sha256: String::new(),
        input: vec![json!({"role":"assistant","content":"disposable review replay ".repeat(2000)})],
    });
    let (selected, report) = plan(&config, &session, &body);
    let candidate = selected.expect("real reviewer-only cleanup should be profitable");
    assert_eq!(candidate.render_primary(), session.render_primary());
    assert!(candidate.review_history.is_none());
    assert_eq!(report["selected_action"], "reviewer_only");
    let row = report["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["candidate_id"] == report["selected_candidate_id"])
        .unwrap();
    assert_eq!(row["removed_groups"], json!([]));
    assert_eq!(row["action"], "reviewer_only");
    output_close(
        row["selection_joint"]["primary_keep"].as_f64().unwrap(),
        row["selection_joint"]["primary_compact"].as_f64().unwrap(),
    );
    let mut no_work = session.clone();
    no_work.review_history = None;
    let (selected, kept) = plan(&config, &no_work, &body);
    assert!(selected.is_none());
    assert_eq!(kept["selected_candidate_id"], "keep");
    assert_eq!(kept["selected_action"], "keep");
}
