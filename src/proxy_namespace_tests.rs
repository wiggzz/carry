// Current-format owner/transcript controls and non-destructive format rejection.
use super::*;

fn digest(value: &impl serde::Serialize) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

fn seed_namespace(
    service: &Service,
    headers: &HeaderMap,
    shared: bool,
    initialized: bool,
) -> (Value, String, String, Value) {
    let mut body = json!({"model":"gpt-6.1-sol", "max_output_tokens":1,
        "prompt_cache_options":{"mode":"implicit","ttl":"30m"},
        "input":[{"role":"assistant","content":"owned evidence"},
                 {"role":"user","content":"current request"}]});
    let mut trajectory = crate::proxy_savings::Trajectory::default();
    let inputs = if initialized {
        vec![300000, 300100, 300200]
    } else {
        vec![10000, 300000, 300100, 300200]
    };
    for (n, input) in inputs.iter().enumerate() {
        if n > 0 {
            body["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("seed append {n}")}));
        }
        trajectory.primary(
            &body,
            &json!({"status":"completed","output":[],
            "usage":{"input_tokens":input,"output_tokens":0,
                "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0}}}),
            &service.config.upstream_url,
            now(),
            true,
        );
    }
    let frozen = serde_json::to_value(&trajectory).unwrap();
    assert_eq!(frozen["retired"], 280000);
    assert_eq!(frozen["summary"], 1000);
    assert_eq!(frozen["last_traditional_input"], 21200);
    assert_eq!(
        trajectory.snapshot().initial_compactions,
        u64::from(initialized)
    );
    assert_eq!(
        trajectory.snapshot().traditional_compactions,
        u64::from(!initialized)
    );
    assert_eq!(trajectory.snapshot().summary_cost > 0.0, !initialized);
    let namespace = history::namespace(service, headers, &body).unwrap();
    let id = identity(headers)
        .unwrap()
        .unwrap_or_else(|| digest(&(namespace.as_str(), "seed-owner")));
    let mut session = Session {
        history_namespace: Some(namespace.clone()),
        history_shared: shared,
        review_context: cache_base(&body),
        cost_trajectory: Some(trajectory),
        ..Session::default()
    };
    session.ingest(body["input"].as_array().unwrap()).unwrap();
    let response = json!({"id":"legacy-observed-response", "status":"completed",
        "output":[{"role":"assistant","content":"actually observed output"}]});
    save_observed(
        service,
        &id,
        &session,
        Some((body["input"].as_array().unwrap(), &response)),
    )
    .unwrap();
    let loaded = load(service, &id).unwrap();
    assert_eq!(
        serde_json::to_value(loaded.cost_trajectory.unwrap()).unwrap(),
        frozen,
        "storage must preserve every frozen quantity and monetary field"
    );
    (body, namespace, id, frozen)
}

// Apply the same genuine scope table to both lookup surfaces. In particular,
// an observed response ID is not a capability to borrow another scope's nodes.
fn assert_namespace_fences(
    service: &Service,
    headers: &HeaderMap,
    body: &Value,
    id: &str,
    frozen: &Value,
) {
    for boundary in ["model", "instructions", "tenant"] {
        let mut different = body.clone();
        let mut other_headers = headers.clone();
        if boundary == "tenant" {
            other_headers.insert("x-carry-tenant", "another-owner".parse().unwrap());
        } else {
            different[boundary] = json!("different-boundary");
        }
        let namespace = history::namespace(service, &other_headers, &different).unwrap();
        let (selected, pin) = history::resolve(service, &namespace, &different).unwrap();
        assert_ne!(selected, id, "{boundary} must fence owner lookup");
        drop(pin);
        different["input"] = json!([{"role":"user","content":"unrelated resume"}]);
        different["previous_response_id"] = json!("legacy-observed-response");
        let unchanged = different.clone();
        let error = history::expand(service, &namespace, &mut different)
            .expect_err("foreign scope must not expand a known external identifier");
        assert_eq!(error.0, StatusCode::CONFLICT, "{boundary} expansion");
        assert_eq!(
            different, unchanged,
            "rejected expansion must not expose ancestry"
        );
        assert_eq!(
            serde_json::to_value(load(service, id).unwrap().cost_trajectory.unwrap()).unwrap(),
            *frozen,
            "scope rejection must leave all historic quantities and money frozen"
        );
    }
}

fn namespace_owner_restart(initialized: bool) {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let headers = HeaderMap::new();
    let (mut body, old_namespace, id, frozen) =
        seed_namespace(&service, &headers, true, initialized);
    let (control, pin) = history::resolve(&service, &old_namespace, &body).unwrap();
    assert_eq!(control, id);
    drop(pin);
    drop(service);
    let service = checkpoint_service(directory.path());
    assert_eq!(
        serde_json::to_value(load(&service, &id).unwrap().cost_trajectory.unwrap()).unwrap(),
        frozen
    );
    body["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"append after restart"}));
    assert_namespace_fences(&service, &headers, &body, &id, &frozen);
    let namespace = history::namespace(&service, &headers, &body).unwrap();
    let (selected, _pin) = history::resolve(&service, &namespace, &body).unwrap();
    assert_eq!(
        selected, id,
        "proven append ancestry must retain its paid/free quantity owner"
    );
    assert_eq!(
        serde_json::to_value(
            history::restore(&service, &selected)
                .unwrap()
                .cost_trajectory
                .unwrap()
        )
        .unwrap(),
        frozen
    );
}

fn namespace_transcript_restart(shared: bool) {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let mut headers = HeaderMap::new();
    if !shared {
        headers.insert("x-carry-session", "upgrade-explicit".parse().unwrap());
    }
    let (body, old_namespace, id, frozen) = seed_namespace(&service, &headers, shared, false);
    let delta = json!({"role":"user","content":"resume after restart"});
    let mut resumed = body.clone();
    resumed["input"] = json!([delta]);
    resumed["previous_response_id"] = json!("legacy-observed-response");
    let mut control = resumed.clone();
    history::expand(&service, &old_namespace, &mut control).unwrap();
    let mut expected = body["input"].as_array().unwrap().clone();
    expected.extend([
        json!({"role":"assistant","content":"actually observed output"}),
        delta,
    ]);
    assert_eq!(control["input"], json!(expected));
    assert!(control.get("previous_response_id").is_none());
    drop(service);
    let service = checkpoint_service(directory.path());
    assert_eq!(
        serde_json::to_value(load(&service, &id).unwrap().cost_trajectory.unwrap()).unwrap(),
        frozen
    );
    assert_namespace_fences(&service, &headers, &body, &id, &frozen);
    let namespace = history::namespace(&service, &headers, &resumed).unwrap();
    history::expand(&service, &namespace, &mut resumed)
        .expect("a successfully observed identifier remains reachable across restart");
    assert_eq!(resumed["input"], json!(expected));
    assert!(resumed.get("previous_response_id").is_none());
}

#[test]
fn reviewer_contract_changes_invalidate_advice_not_observed_history() {
    let mut failures = Vec::new();
    for boundary in [
        "reviewer_model",
        "reviewer_url",
        "reasoning",
        "reviewer_output",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        let headers = HeaderMap::new();
        let (body, namespace, id, frozen) = seed_namespace(&service, &headers, true, false);
        let subject = history::restore(&service, &id).unwrap();
        history::publish(&service, &subject, &subject).unwrap();
        let input = body["input"].as_array().unwrap();
        assert!(
            history::advice(&service, &namespace, input)
                .unwrap()
                .is_some()
        );
        drop(service);
        let mut service = checkpoint_service(directory.path());
        assert!(
            history::advice(&service, &namespace, input)
                .unwrap()
                .is_some()
        );
        match boundary {
            "reviewer_model" => service.config.classifier_model = "another-reviewer".into(),
            "reviewer_url" => {
                service.config.classifier_url = Some("http://127.0.0.1:1/other-reviewer".into())
            }
            "reasoning" => service.config.classifier_reasoning_effort = "high".into(),
            "reviewer_output" => service.config.classifier_max_output_tokens += 1,
            _ => unreachable!(),
        }
        let changed_namespace = history::namespace(&service, &headers, &body).unwrap();
        let (selected, pin) = history::resolve(&service, &changed_namespace, &body).unwrap();
        let advice = history::advice(&service, &changed_namespace, input).unwrap();
        drop(pin);
        assert_eq!(
            serde_json::to_value(load(&service, &id).unwrap().cost_trajectory.unwrap()).unwrap(),
            frozen,
            "reviewer policy cannot rewrite historic money or quantities"
        );
        if selected != id {
            failures.push(format!("{boundary}: renamed the observed history owner"));
        }
        if advice.is_some() {
            failures.push(format!("{boundary}: reused incompatible completed advice"));
        }
    }
    assert!(
        failures.is_empty(),
        "HISTORY_ADVICE_VALIDITY: {}",
        failures.join("; ")
    );
}

// Will withdrew predecessor-format continuation. Keep the current-format
// restart control, but require unsupported persisted authority to be rejected
// without migration, reinitialization or modification of any saved receipts.
#[test]
fn unsupported_namespace_rejects_without_rewriting_persisted_authority() {
    let mut failures = Vec::new();
    for legacy in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        let headers = HeaderMap::new();
        let (body, original_namespace, id, _frozen) =
            seed_namespace(&service, &headers, true, false);
        drop(service);
        let index_path = directory.path().join("history.index");
        let mirror_path = directory.path().join(format!("{id}.json"));
        if legacy {
            // Stage unsupported bytes directly: rejection needs no old producer,
            // compatibility decoder or successful load of predecessor state.
            let mut index: Value =
                serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
            index["format"] = json!("carry-prefix-history-v3");
            std::fs::write(&index_path, serde_json::to_vec(&index).unwrap()).unwrap();
        }
        let index = std::fs::read(&index_path).unwrap();
        let mirror = std::fs::read(&mirror_path).unwrap();
        for restart in 0..2 {
            let service = checkpoint_service(directory.path());
            let outcome = history::namespace(&service, &headers, &body);
            match outcome {
                Ok(namespace) if !legacy => assert_eq!(namespace, original_namespace),
                Err((status, axum::Json(error))) if legacy => {
                    if status != StatusCode::CONFLICT
                        || !error["error"]["message"]
                            .as_str()
                            .is_some_and(|message| message.contains("unsupported"))
                    {
                        failures.push(format!(
                            "restart={restart}: not a clear unsupported-state rejection"
                        ));
                    }
                }
                _ => failures.push(format!(
                    "legacy={legacy}, restart={restart}: unexpected acceptance/rejection"
                )),
            }
            if std::fs::read(&index_path).unwrap() != index {
                failures.push(format!(
                    "legacy={legacy}, restart={restart}: rewrote authoritative index"
                ));
            }
            assert_eq!(std::fs::read(&mirror_path).unwrap(), mirror);
        }
    }
    assert!(
        failures.is_empty(),
        "UNSUPPORTED_NAMESPACE_PERSISTENCE: {}",
        failures.join("; ")
    );
}

// The supported index envelope must not make an older trajectory safe to load.
// Exercise both explicit-file authority and canonical shared-index authority;
// unsupported accounting state is an error, never a new/read-only history.
#[test]
fn unsupported_receipt_history_rejects_before_loading_persisted_authority() {
    let mut failures = Vec::new();
    for initialized in [true, false] {
        for shared in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let service = checkpoint_service(directory.path());
            let mut headers = HeaderMap::new();
            if !shared {
                headers.insert("x-carry-session", "receipt-format-control".parse().unwrap());
            }
            let (_, _, id, frozen) = seed_namespace(&service, &headers, shared, initialized);
            drop(service);
            let index_path = directory.path().join("history.index");
            let mirror_path = directory.path().join(format!("{id}.json"));
            let index = std::fs::read(&index_path).unwrap();
            let mirror = std::fs::read(&mirror_path).unwrap();
            // Supported current restart retains every paid/free quantity and
            // frozen monetary field, without normalizing or writing either file.
            for _ in 0..2 {
                let service = checkpoint_service(directory.path());
                let loaded = load(&service, &id).unwrap();
                assert_eq!(
                    serde_json::to_value(loaded.cost_trajectory.unwrap()).unwrap(),
                    frozen
                );
                assert_eq!(std::fs::read(&index_path).unwrap(), index);
                assert_eq!(std::fs::read(&mirror_path).unwrap(), mirror);
            }
            for shape in [
                "opaque_anchor",
                "missing_cursor_basis",
                "raw_cursor",
                "read_only_legacy",
            ] {
                let mut unsupported = frozen.clone();
                match shape {
                    "opaque_anchor" => {
                        unsupported
                            .as_object_mut()
                            .unwrap()
                            .remove("history_continuity");
                        unsupported
                            .as_object_mut()
                            .unwrap()
                            .remove("receipt_history_load");
                        unsupported
                            .as_object_mut()
                            .unwrap()
                            .remove("shared_primary_history");
                        unsupported["receipts"]["history_anchor"] =
                            unsupported["receipts"]["anchor"].clone();
                    }
                    "missing_cursor_basis" => {
                        unsupported["history_continuity"]["cursor"]
                            .as_object_mut()
                            .unwrap()
                            .remove("basis");
                    }
                    "raw_cursor" => {
                        unsupported["history_continuity"]["cursor"]["basis"] = json!("RawV1")
                    }
                    "read_only_legacy" => {
                        unsupported["receipt_history_load"] = json!("ReadOnlyLegacyV1")
                    }
                    _ => unreachable!(),
                }
                if shared {
                    let mut saved: Value = serde_json::from_slice(&index).unwrap();
                    saved["snapshots"][&id]["cost_trajectory"] = unsupported;
                    std::fs::write(&index_path, serde_json::to_vec(&saved).unwrap()).unwrap();
                    std::fs::write(&mirror_path, &mirror).unwrap();
                } else {
                    let mut saved: Value = serde_json::from_slice(&mirror).unwrap();
                    saved["cost_trajectory"] = unsupported;
                    std::fs::write(&mirror_path, serde_json::to_vec(&saved).unwrap()).unwrap();
                    std::fs::write(&index_path, &index).unwrap();
                }
                let staged_index = std::fs::read(&index_path).unwrap();
                let staged_mirror = std::fs::read(&mirror_path).unwrap();
                for restart in 0..2 {
                    let service = checkpoint_service(directory.path());
                    match load(&service, &id) {
                        Ok(_) => failures.push(format!("initialized={initialized}, shared={shared}, shape={shape}, restart={restart}: accepted unsupported history")),
                        Err(error) => {
                            if !error.1["error"]["message"].as_str().is_some_and(|m| m.contains("unsupported")) {
                                failures.push(format!("initialized={initialized}, shared={shared}, shape={shape}, restart={restart}: unclear unsupported-state error"));
                            }
                        }
                    }
                    assert_eq!(std::fs::read(&index_path).unwrap(), staged_index);
                    assert_eq!(std::fs::read(&mirror_path).unwrap(), staged_mirror);
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "UNSUPPORTED_RECEIPT_HISTORY: {}",
        failures.join("; ")
    );
}

#[test]
fn namespace_restart_preserves_paid_history_owner() {
    namespace_owner_restart(false);
}

#[test]
fn namespace_restart_preserves_initialized_history_owner() {
    namespace_owner_restart(true);
}

#[test]
fn namespace_restart_preserves_implicit_observed_transcript() {
    namespace_transcript_restart(true);
}

#[test]
fn namespace_restart_preserves_explicit_observed_transcript() {
    namespace_transcript_restart(false);
}

#[test]
fn namespace_preparation_is_read_only_at_exact_capacity() {
    // Lookup cannot rewrite or evict current authority, even at its byte bound.
    for roomy in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        let headers = HeaderMap::new();
        let (body, old_namespace, id, frozen_money) =
            seed_namespace(&service, &headers, true, false);
        drop(service);
        let mut service = checkpoint_service(directory.path());
        let index_path = directory.path().join("history.index");
        let mirror_path = directory.path().join(format!("{id}.json"));
        let before = std::fs::read(&index_path).unwrap();
        let mirror = std::fs::read(&mirror_path).unwrap();
        let old: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(old["tips"].as_object().unwrap().len(), 1);
        assert_eq!(old["transcripts"].as_object().unwrap().len(), 1);
        assert_eq!(old["snapshots"][&id]["cost_trajectory"], frozen_money);
        service.config.history_max_bytes =
            u32::try_from(before.len() + if roomy { 8192 } else { 0 }).unwrap();
        let outcome = history::namespace(&service, &headers, &body);
        let after = std::fs::read(&index_path).unwrap();
        let current: Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(current["snapshots"][&id]["cost_trajectory"], frozen_money);
        assert_eq!(std::fs::read(&mirror_path).unwrap(), mirror);
        let namespace = outcome.unwrap();
        assert_eq!(namespace, old_namespace);
        assert_eq!(after, before);
        assert_eq!(current["tips"].as_object().unwrap().len(), 1);
        assert_eq!(current["transcripts"].as_object().unwrap().len(), 1);
        let transcript = current["transcripts"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        let original = old["transcripts"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(transcript["at"], original["at"]);
        assert_eq!(transcript["session"], original["session"]);
        let mut resumed = body.clone();
        resumed["input"] = json!([]);
        resumed["previous_response_id"] = json!("legacy-observed-response");
        history::expand(&service, &namespace, &mut resumed).unwrap();
        assert_eq!(
            resumed["input"].as_array().unwrap().len(),
            body["input"].as_array().unwrap().len() + 1
        );
        assert_eq!(
            history::namespace(&service, &headers, &body).unwrap(),
            namespace
        );
        assert_eq!(std::fs::read(&index_path).unwrap(), after);
        drop(service);
        let service = checkpoint_service(directory.path());
        assert_eq!(
            history::namespace(&service, &headers, &body).unwrap(),
            namespace
        );
        assert_eq!(std::fs::read(&index_path).unwrap(), after);
    }
}

// Exercise the current production request/settlement path across restart,
// alternating observed response/conversation IDs with omitted repeated metadata.
#[tokio::test]
async fn namespace_restart_alternating_ids_retains_explicit_conversation() {
    let received = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let captured = received.clone();
    let app = Router::new().route(
        "/responses",
        post(move |axum::Json(body): axum::Json<Value>| {
            let captured = captured.clone();
            async move {
                let mut requests = captured.lock().unwrap();
                requests.push(body);
                // Deliberately omit repeated conversation metadata.
                axum::Json(json!({"id":format!("continued-{}", requests.len()),
                    "status":"completed", "output":[{"role":"assistant","content":"new observed output"}],
                    "usage":{"input_tokens":100,"output_tokens":1,
                        "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0}}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    struct StopServer(tokio::task::AbortHandle);
    impl Drop for StopServer {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = StopServer(server.abort_handle());
    received.lock().unwrap().clear();
    let directory = tempfile::tempdir().unwrap();
    let make_service = || {
        let mut service = checkpoint_service(directory.path());
        service.config.upstream_url = url.clone();
        service.config.mode = Mode::Audit;
        service.config.review_min_primary_tokens = 1_000_000;
        service
    };
    let service = make_service();
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "alternating-owner".parse().unwrap());
    let (body, _old_namespace, id, frozen) = seed_namespace(&service, &headers, false, false);
    let mut seed = load(&service, &id).unwrap();
    seed.history_conversation = Some("observed-conversation".into());
    save_observed(
        &service,
        &id,
        &seed,
        Some((
            body["input"].as_array().unwrap(),
            &json!({
                "id":"legacy-observed-response", "status":"completed",
                "conversation":{"id":"observed-conversation"},
                "output":[{"role":"assistant","content":"actually observed output"}]}),
        )),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(seed.cost_trajectory.unwrap()).unwrap(),
        frozen
    );
    drop(service);
    let service = Arc::new(make_service());
    assert_eq!(
        serde_json::to_value(load(&service, &id).unwrap().cost_trajectory.unwrap()).unwrap(),
        frozen
    );
    let delta = json!({"role":"user","content":"continue by response id"});
    let mut resumed = body.clone();
    resumed["input"] = json!([delta]);
    resumed["previous_response_id"] = json!("legacy-observed-response");
    let response = forward(
        service.clone(),
        headers.clone(),
        Bytes::from(serde_json::to_vec(&resumed).unwrap()),
        false,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let raw = axum::body::to_bytes(response.into_body(), 1_048_576)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&raw).unwrap()["id"],
        "continued-1"
    );
    let middle = load(&service, &id).unwrap();
    assert_eq!(middle.primary.calls, 1);
    assert!((middle.primary.cost_usd - 0.00021).abs() < 1e-12);
    assert_eq!(middle.shadow.calls, 0);
    let mut expected = body["input"].as_array().unwrap().clone();
    expected.extend([
        json!({"role":"assistant","content":"actually observed output"}),
        delta,
    ]);
    {
        let requests = received.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["input"], json!(expected));
        assert!(requests[0].get("previous_response_id").is_none());
        assert!(requests[0].get("conversation").is_none());
    }
    drop(service);
    let service = Arc::new(make_service());
    let third = json!({"role":"user","content":"continue by conversation id"});
    resumed
        .as_object_mut()
        .unwrap()
        .remove("previous_response_id");
    resumed["input"] = json!([third]);
    resumed["conversation"] = json!("observed-conversation");
    let outcome = forward(
        service.clone(),
        headers.clone(),
        Bytes::from(serde_json::to_vec(&resumed).unwrap()),
        false,
    )
    .await;
    let status = match &outcome {
        Ok(response) => response.status(),
        Err(failure) => failure.0,
    };
    if status != StatusCode::OK {
        assert_eq!(
            received.lock().unwrap().len(),
            1,
            "rejection cannot send another paid request"
        );
        let after = load(&service, &id).unwrap();
        assert_eq!(
            serde_json::to_value(after.cost_trajectory.unwrap()).unwrap(),
            serde_json::to_value(middle.cost_trajectory.unwrap()).unwrap()
        );
        assert_eq!(after.primary.calls, 1);
        assert!((after.primary.cost_usd - 0.00021).abs() < 1e-12);
    }
    assert_eq!(
        status,
        StatusCode::OK,
        "proven conversation must advance when the next response omits repeated metadata"
    );
    let response = outcome.unwrap();
    axum::body::to_bytes(response.into_body(), 1_048_576)
        .await
        .unwrap();
    expected.extend([
        json!({"role":"assistant","content":"new observed output"}),
        third,
    ]);
    let requests = received.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["input"], json!(expected));
    assert!(requests[1].get("conversation").is_none());
}
