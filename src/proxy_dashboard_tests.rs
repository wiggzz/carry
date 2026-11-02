#[tokio::test]
async fn dashboard_failure_only_describes_the_latest_failed_primary() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let body = json!({"model":"gpt-6-luna", "input":[]});
    for (n, outcome, failure_request, visible) in [
        (0, "completed", Some("old"), false),
        (1, "failed", Some("old"), false),
        (2, "failed", Some("latest"), true),
        (3, "dispatched", Some("old"), false),
        (4, "failed", None, false),
    ] {
        let mut session = Session {
            failed_primaries: 3,
            last_failure: Some(json!({"reason":"old failure"})),
            last_primary_dispatch: Some(dispatch_measurement("latest", "primary", &body)),
            ..Session::default()
        };
        session.last_primary_dispatch.as_mut().unwrap().outcome = outcome.into();
        if let Some(request_id) = failure_request {
            session.last_failure.as_mut().unwrap()["request_id"] = json!(request_id);
        }
        let id = format!("{n:064x}");
        save(&service, &id, &session).unwrap();
        let response = dashboard_stats(
            State(service.clone()),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let stats: Value = serde_json::from_slice(&bytes).unwrap();
        let projected = stats["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["id"] == id)
            .unwrap();
        assert_eq!(
            projected["last_failure"].is_object(),
            visible,
            "{outcome}, {failure_request:?}"
        );
        assert_eq!(projected["failed_primaries"], 3);
        assert_eq!(
            load_existing(&service, &id).unwrap().last_failure,
            session.last_failure
        );
    }
}

#[test]
fn dashboard_paid_fallback_settles_only_the_matching_primary_failure() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let body = json!({"model":"gpt-6-luna", "input":[]});
    let retained = Session {
        last_failure: Some(json!({"request_id":"old", "reason":"old failure"})),
        last_primary_dispatch: Some(dispatch_measurement("latest", "primary", &body)),
        failed_primaries: 2,
        ..Session::default()
    };
    for (request, completed, copied) in [
        ("latest", false, true),
        ("latest", true, true),
        ("unrelated", false, false),
    ] {
        save(&service, "failure-fallback", &retained).unwrap();
        let mut paid = retained.clone();
        finish_measurement(&mut paid.last_primary_dispatch, None, completed);
        paid.last_failure =
            (!completed).then(|| json!({"request_id":"latest", "reason":"new failure"}));
        save_paid_accounting(&service, "failure-fallback", &paid, request).unwrap();
        let actual = load_existing(&service, "failure-fallback").unwrap();
        assert_eq!(
            actual.last_failure,
            if copied {
                paid.last_failure
            } else {
                retained.last_failure.clone()
            }
        );
        assert_eq!(actual.failed_primaries, 2);
    }
}

#[test]
fn dashboard_counts_reuse_equivalent_prompts_without_repricing_receipts() {
    let base = json!({"model":"gpt-6-luna", "input":[
        {"role":"user","content":"large owned source 🦀 ".repeat(1000)},
        {"type":"function_call_output","call_id":"opaque","output":"{\"text\":\"literal\"}"}
    ], "instructions":"fixed instruction", "tools":[{"type":"function","name":"fixture","parameters":{"type":"object"}}],
    "text":{"format":{"type":"json_object"}}});
    let mut transport = base.clone();
    transport["model"] = json!("other-model");
    transport["max_output_tokens"] = json!(900);
    transport["prompt_cache_key"] = json!("other-affinity");
    transport["stream"] = json!(true);
    let mut changed = base.clone();
    changed["instructions"] = json!("genuinely different instruction");
    let adapted = codex_body(&base);
    let invalid = json!({"input":"not an array"});
    let bodies = [&base, &transport, &changed, &adapted, &invalid];
    let mut counts = PromptTokenCounts::default();
    for body in bodies {
        assert_eq!(
            counts.count(body),
            crate::savings::prompt_projection(body).map(|p| p.2)
        );
    }
    let mut equivalent = PromptTokenCounts::default();
    let before = equivalent.count(&base);
    assert_eq!(equivalent.count(&transport), before);
    assert_eq!(
        equivalent.0.len(),
        1,
        "the equivalent view executes no second tokenizer pass"
    );
    assert_eq!(
        equivalent.count(&changed),
        crate::savings::prompt_input_tokens(&changed)
    );
    assert_eq!(equivalent.0.len(), 2);
    crate::prompt_quantity::reset_token_work();
    let dispatched = dispatch_measurement("request", "primary", &base);
    assert_eq!(dispatched.outgoing_input_tokens, None,
        "constructing telemetry must not eagerly count a prompt");
    assert_eq!(crate::prompt_quantity::token_work(), (0, 0));
    assert_eq!(
        dispatched.receipt_input_tokens, None,
        "an estimate is never a provider receipt"
    );
}

#[tokio::test]
async fn dashboard_read_view_preserves_authority_and_reloads_each_poll() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let mut ids = Vec::new();
    for n in 0..7 {
        let id = format!("{n:064x}");
        let mut session = Session {
            history_shared: true,
            history_namespace: Some("owned-dashboard".into()),
            ..Session::default()
        };
        observe_source(&mut session, &format!("shared retained source {n}"));
        save(&service, &id, &session).unwrap();
        ids.push(id);
    }
    let index_path = directory.path().join("history.index");
    let frozen = std::fs::read(&index_path).unwrap();
    let view = history::ReadView::new(&service).unwrap();
    for id in &ids {
        let restored = view.restore(id).unwrap();
        let expected = load(&service, id).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let input = restored
            .history
            .iter()
            .map(|i| i.value.clone())
            .collect::<Vec<_>>();
        assert!(view.advice("owned-dashboard", &input).is_none());
    }
    let poll = || {
        dashboard_stats(
            State(service.clone()),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            HeaderMap::new(),
        )
    };
    let response = poll().await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let stats: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stats["sessions"].as_array().unwrap().len(), 7);
    assert_eq!(stats["unavailable_sessions"], 0);
    assert_eq!(
        std::fs::read(&index_path).unwrap(),
        frozen,
        "a poll cannot rewrite authority"
    );

    // A new poll must not reuse a previously valid view or revive shared history
    // from its diagnostic checkpoint when the authoritative index is broken.
    std::fs::write(&index_path, b"invalid authoritative index").unwrap();
    let response = poll().await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let stats: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(stats["sessions"].as_array().unwrap().is_empty());
    assert_eq!(stats["unavailable_sessions"], 7);
    assert_eq!(
        std::fs::read(&index_path).unwrap(),
        b"invalid authoritative index"
    );
}

#[test]
fn dashboard_read_view_rechecks_expiry_pins_and_capacity_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    let ids = ["a".repeat(64), "b".repeat(64)];
    for (n, id) in ids.iter().enumerate() {
        let mut session = Session {
            history_shared: true,
            history_namespace: Some("view-retention".into()),
            ..Session::default()
        };
        observe_source(&mut session, &format!("retained owner {n}"));
        save(&service, id, &session).unwrap();
    }
    let path = directory.path().join("history.index");
    let original = std::fs::read(&path).unwrap();
    let old_view = history::ReadView::new(&service).unwrap();
    let mut expired: Value = serde_json::from_slice(&original).unwrap();
    for reference in expired["tips"].as_object_mut().unwrap().values_mut() {
        reference["at"] = json!(0);
    }
    let staged = serde_json::to_vec(&expired).unwrap();
    std::fs::write(&path, &staged).unwrap();
    let pin = history::pin(&service, &ids[0]).unwrap();
    let pinned = history::ReadView::new(&service).unwrap();
    assert!(pinned.shared(&ids[0]).unwrap().is_some());
    assert!(pinned.shared(&ids[1]).is_err());
    drop(pin);
    let released = history::ReadView::new(&service).unwrap();
    assert!(released.shared(&ids[0]).is_err());
    assert!(
        old_view.shared(&ids[1]).unwrap().is_some(),
        "an existing view is one coherent generation"
    );
    assert_eq!(std::fs::read(&path).unwrap(), staged);

    std::fs::write(&path, &original).unwrap();
    service.config.history_max_nodes = 1;
    let pin = history::pin(&service, &ids[0]).unwrap();
    let bounded = history::ReadView::new(&service).unwrap();
    assert!(bounded.shared(&ids[0]).unwrap().is_some());
    assert!(bounded.shared(&ids[1]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    drop(pin);
}

#[test]
fn dashboard_paid_fallback_fences_terminal_measurements_by_request_and_lane() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let body = json!({"model":"gpt-6-luna","input":[{"role":"user","content":"retained source"}]});
    let mut retained = Session::default();
    observe_source(&mut retained, "retained source");
    retained.last_primary_dispatch = Some(dispatch_measurement("newer-primary", "primary", &body));
    retained.last_reviewer_dispatch =
        Some(dispatch_measurement("review-parent", "reviewer", &body));
    let mut paid = retained.clone();
    paid.shadow.calls = 1;
    let record = paid.last_reviewer_dispatch.as_mut().unwrap();
    record.request_id = "review-attempt".into();
    record.parent_request_id = Some("review-parent".into());
    finish_measurement(
        &mut paid.last_reviewer_dispatch,
        Some(&json!({"usage":{"input_tokens":100,"output_tokens":1}})),
        true,
    );
    paid.last_primary_dispatch = Some(dispatch_measurement("unrelated-primary", "primary", &body));
    finish_measurement(&mut paid.last_primary_dispatch, None, false);
    paid.title = Some("failed-publication-material".into());
    for (request, lane, copied) in [
        ("review-parent", "reviewer", true),
        ("other-parent", "reviewer", false),
        ("review-parent", "primary", false),
    ] {
        save(&service, "fallback", &retained).unwrap();
        paid.last_reviewer_dispatch.as_mut().unwrap().lane = lane.into();
        save_paid_accounting(&service, "fallback", &paid, request).unwrap();
        let actual = load_existing(&service, "fallback").unwrap();
        assert_eq!(actual.shadow.calls, 1);
        assert_eq!(actual.title, retained.title);
        assert_eq!(
            actual.last_primary_dispatch.unwrap().request_id,
            "newer-primary"
        );
        let reviewer = actual.last_reviewer_dispatch.unwrap();
        assert_eq!(
            reviewer.outcome,
            if copied { "completed" } else { "dispatched" }
        );
        assert_eq!(reviewer.receipt_input_tokens, copied.then_some(100));
    }
}

#[tokio::test]
async fn dashboard_transcript_uses_real_private_peer_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.auth_token = Some("fixture-dashboard-token".into());
    let service = Arc::new(service);
    let id = "a".repeat(64);
    let mut session = Session::default();
    observe_source(&mut session, "owned private source");
    save(&service, &id, &session).unwrap();
    for peer in ["192.0.2.1:1234", "[2001:db8::1]:1234"] {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
        headers.insert("host", "localhost:8787".parse().unwrap());
        let failure = dashboard_transcript(
            State(service.clone()),
            ConnectInfo(peer.parse().unwrap()),
            headers,
            axum::extract::Path(id.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.0, StatusCode::UNAUTHORIZED);
    }
    let response = dashboard_transcript(
        State(service),
        ConnectInfo("127.0.0.1:1".parse().unwrap()),
        HeaderMap::new(),
        axum::extract::Path(id),
    )
    .await
    .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let transcript: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        transcript["canonical_input"][0]["content"],
        "owned private source"
    );
}

#[test]
fn dashboard_measurement_retains_provider_output_without_inventing_missing_usage() {
    let body = json!({"input":[]});
    let mut record = Some(dispatch_measurement("receipt", "primary", &body));
    finish_measurement(
        &mut record,
        Some(&json!({"usage":{"input_tokens":120,"output_tokens":37}})),
        true,
    );
    let encoded = serde_json::to_value(&record).unwrap();
    assert_eq!(encoded["receipt_output_tokens"], 37);
    finish_measurement(
        &mut record,
        Some(&json!({"usage":{"input_tokens":120}})),
        false,
    );
    assert!(serde_json::to_value(&record).unwrap()["receipt_output_tokens"].is_null());
    // Older saved numeric measurements remain readable with output unknown.
    let mut old = encoded;
    old.as_object_mut().unwrap().remove("receipt_output_tokens");
    let decoded: crate::proxy_state::DispatchMeasurement = serde_json::from_value(old).unwrap();
    assert!(serde_json::to_value(decoded).unwrap()["receipt_output_tokens"].is_null());
}

#[tokio::test]
async fn dashboard_context_states_partition_sources_not_memories() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let mut session = Session {
        history_namespace: Some("context-partition".into()),
        ..Session::default()
    };
    for n in 0..5 {
        observe_source(&mut session, &format!("source {n}"));
    }
    session.apply_advice(&session.groups(), &json!({"removable":[1,2],"protected":[3],"memories":[{"source_ids":[1],"text":"Remember source one"}]})).unwrap();
    session.last_submitted_ids = vec![2, 3, 4, 5];
    history::publish(&service, &session, &session).unwrap();
    save(&service, &"c".repeat(64), &session).unwrap();
    let response = dashboard_stats(
        State(service),
        ConnectInfo("127.0.0.1:1".parse().unwrap()),
        HeaderMap::new(),
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let stats: Value = serde_json::from_slice(&bytes).unwrap();
    let c = &stats["sessions"][0]["context"];
    assert_eq!(c["history_items"], 5);
    assert_eq!(c["removed_items"], 1);
    assert_eq!(
        c["removable_items"], 1,
        "omitted authorized sources are not double counted as removable"
    );
    assert_eq!(c["protected_items"], 2);
    assert_eq!(c["unreviewed_items"], 1);
    assert_eq!(
        c["memories"], 1,
        "memory facts are separate from source states"
    );
}
