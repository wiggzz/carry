// Actual raw-render entrypoint counts, not a latency benchmark. Forecast-owned
// renders remain visible in total counts but are outside the explicit-loop seam.
fn render_reuse_memory_fixture(release: bool) -> (ProxyCli, Session, Value) {
    let (mut config, mut session, _) = lease_fixture();
    config.payoff_requests = 1;
    config.review_min_new_tokens = 0;
    session
        .apply_advice(
            &session.groups(),
            &json!({
                "protected":[],"removable":["g2"],
                "memories":[{"source_ids":["g2"],"text":"durable compact finding"}]
            }),
        )
        .unwrap();
    session.remove(&[2], &session.groups());
    assert!(session.history[1].removed);
    let memory = format!("m{}", session.memories[0].id);
    session
        .apply_advice(
            &session.groups(),
            &json!({
                "protected":[memory],"removable":[],"memories":[]
            }),
        )
        .unwrap();
    if release {
        session.completed_requests += 8;
        let pending = crate::proxy_state::PendingKeepReview {
            request_id: "render-reuse-memory".into(),
            targets: session.due_keep_targets(),
        };
        assert_eq!(pending.targets.len(), 1);
        session.resolve_keep_targets(&pending);
        assert_eq!(session.released_memory_ids(), vec![session.memories[0].id]);
    }
    let body = json!({"model":"gpt-6-sol","input":session.render_primary()});
    (config, session, body)
}

fn render_reuse_codex_fixture() -> (ProxyCli, Session, Value) {
    let (mut config, mut session, _) = lease_work_fixture(1, false);
    config.codex_login = true;
    // Stage leading native instruction items; keep whole tool cohorts intact.
    session.history[0].value["role"] = json!("system");
    let id = session.next_id;
    session.next_id += 1;
    session.history.insert(
        1,
        crate::proxy_state::Item {
            id,
            value: json!({"role":"developer","content":"native instruction control"}),
            cohort: id,
            exposed: true,
            removed: false,
        },
    );
    // A genuine disposable replay makes the keep-all reviewer-only row legal.
    session.review_history = Some(crate::proxy_state::ReviewHistory {
        source_len: 0,
        source_sha256: reviewer_serialized_identity(&Vec::<Value>::new()).1,
        input: vec![json!({"role":"assistant","content":"disposable review turn"})],
    });
    let body = json!({"model":"gpt-6-sol","prompt_cache_key":"render-reuse-stable",
        "input":session.render_primary()});
    let wire = primary_wire_body(&config, &body);
    assert_ne!(wire["input"], body["input"]);
    session.primary_cache.clear();
    for length in [4, 7, 10, 13, 16, 19, 22, 25] {
        let mut prefix = wire.clone();
        prefix["input"].as_array_mut().unwrap().truncate(length);
        observe_cache(
            &mut session.primary_cache,
            &prefix,
            &json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":100}}),
        );
    }
    (config, session, body)
}

fn render_reuse_snapshot(config: &ProxyCli, session: &Session, body: &Value) -> Value {
    let mut raw = body.clone();
    raw["input"] = json!(session.render_primary());
    let mut state = session.clone();
    // Receipt timestamps differ between hosted runs. Only this wall-clock field
    // is normalized for cross-run state comparison, after real planning/TTL checks.
    for receipt in &mut state.primary_cache {
        receipt.at = 0;
    }
    let protection = session.groups().iter().map(|g| json!({
        "id":g.id,"members":g.members,"pinned":g.pinned,"human":g.human,
        "replayable":g.replayable,"exposed":g.exposed,"drop_authorized":session.drop_authorized(g)
    })).collect::<Vec<_>>();
    let output = json!({"state":state,"primary_raw":raw,
        "primary_wire":primary_wire_body(config, &raw),
        "reviewer":review_body(config, session),"protection":protection});
    json!({"sha256":format!("{:x}", Sha256::digest(serde_json::to_vec(&output).unwrap()))})
}

#[test]
fn primary_render_reuse_counts_and_semantics() {
    use std::io::Write;
    let mut violations = Vec::new();
    for (name, fixture) in [
        ("six_actions", lease_work_fixture(1, false)),
        ("codex_leading_instructions", render_reuse_codex_fixture()),
        (
            "materialized_protected_memory",
            render_reuse_memory_fixture(false),
        ),
        ("retired_memory", render_reuse_memory_fixture(true)),
    ] {
        let (config, session, body) = fixture;
        let before = serde_json::to_value(&session).unwrap();
        planner_work::start();
        let (candidate, report) = plan(&config, &session, &body);
        let counts = planner_work::finish();
        assert_eq!(serde_json::to_value(&session).unwrap(), before);
        assert_eq!(
            counts["plans"], 1,
            "fixture must isolate one nonnested candidate loop"
        );
        let actions = report["candidates"].as_array().unwrap();
        if name == "six_actions" {
            assert_eq!(actions.len(), 6);
            assert!(candidate.is_some());
        }
        if name == "codex_leading_instructions" {
            assert!(actions.iter().any(|a| a["action"] == "reviewer_only"));
            let selected = candidate.as_ref().unwrap_or(&session).render_primary();
            assert_eq!(selected[0]["role"], "system");
            assert_eq!(selected[1]["role"], "developer");
        }
        if name == "materialized_protected_memory" {
            assert_eq!(candidate.as_ref().unwrap_or(&session).memories.len(), 1);
        }
        if name == "retired_memory" {
            let selected = candidate
                .as_ref()
                .expect("released memory must retire economically");
            assert!(selected.memories.is_empty());
            assert!(
                selected.history[1].removed,
                "retirement never resurrects raw source"
            );
        }
        // An actionless plan needs no cached original. Never pretend this bound
        // covers forecast-owned renders, the ID-aligned cache view, or nested plans.
        let expected = if actions.is_empty() {
            0
        } else {
            actions.len() as u64 + 1
        };
        let observed = counts
            .get("candidate_primary_renders")
            .copied()
            .unwrap_or(0);
        writeln!(std::io::stdout(), "PRIMARY_RENDER_REUSE {}", json!({
            "scenario":name,"counts":counts,"actions":actions.len(),"expected_loop_renders":expected,
            "report":report,"source":render_reuse_snapshot(&config,&session,&body),
            "selected":render_reuse_snapshot(&config,candidate.as_ref().unwrap_or(&session),&body),
            "selected_some":candidate.is_some(),"boundary":"hosted_debug_direct_component_not_HTTP"
        })).unwrap();
        if observed != expected {
            violations.push(format!("{name}: loop {observed} != {expected}"));
        }
    }
    // Exercise the real acquisition probe with positive and paid-negative outcomes.
    for (name, paid, expected_selected) in [
        ("economic_positive", 0.0, true),
        ("economic_paid_negative", 1_000_000.0, false),
    ] {
        let (config, session, body) = lease_work_fixture(1, false);
        let before = serde_json::to_value(&session).unwrap();
        planner_work::start();
        let selected = super::super::planning::economic_candidate(&config, &session, &body, paid);
        let counts = planner_work::finish();
        assert_eq!(serde_json::to_value(&session).unwrap(), before);
        assert_eq!(selected, expected_selected);
        assert_eq!(counts["plans"], 1);
        assert_eq!(counts["economic_probes"], 1);
        let expected = if selected { 2 } else { 1 };
        let observed = counts["economic_primary_renders"];
        writeln!(std::io::stdout(), "PRIMARY_RENDER_REUSE {}", json!({
            "scenario":name,"counts":counts,"selected":selected,"expected_economic_renders":expected
        })).unwrap();
        if observed != expected {
            violations.push(format!("{name}: economic {observed} != {expected}"));
        }
    }
    assert!(
        violations.is_empty(),
        "PRIMARY_RENDER_REUSE_RED: {violations:?}"
    );
}
