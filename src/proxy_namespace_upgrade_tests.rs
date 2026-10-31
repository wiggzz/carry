// Exact predecessor namespace fixture, not an old-executable upgrade test.
// Actual predecessor -> candidate CLI evidence is retained separately. These
// mandatory tests keep that lookup boundary covered without downloading old
// executables or making authentication/network access part of the test suite.
use super::*;

fn digest(value: &impl serde::Serialize) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

// Frozen 19a0bb7 producer: history-v3 wraps advice-v3(cache_base(body)).
// Do not use the current namespace producer to construct a legacy positive.
fn predecessor_namespace(service: &Service, headers: &HeaderMap, body: &Value) -> String {
    let mut owner = headers.clone();
    if identity(headers).unwrap().is_none() {
        owner.insert("x-carry-session", "carry-active-prefix".parse().unwrap());
    }
    let mut context = cache_base(body);
    context.as_object_mut().unwrap().remove("prompt_cache_key");
    let config = &service.config;
    let settings = digest(&json!({
        "policy":"carry-prefix-advice-v3", "context":context,
        "upstream":config.upstream_url, "subscription":config.codex_login,
        "reviewer":config.classifier_model, "reviewer_url":config.classifier_url,
        "reasoning":config.classifier_reasoning_effort,
        "reviewer_output":config.classifier_max_output_tokens
    }));
    digest(
        &json!({"policy":"carry-prefix-history-v3", "owner":identity(&owner).unwrap(),
        "settings":settings, "client_scope":stable_client_scope(body)}),
    )
}

fn seed_namespace(
    service: &Service,
    headers: &HeaderMap,
    shared: bool,
    initialized: bool,
    legacy: bool,
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
    let namespace = if legacy {
        predecessor_namespace(service, headers, &body)
    } else {
        history::namespace(service, headers, &body).unwrap()
    };
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
        "legacy storage/control must preserve every frozen quantity and monetary field"
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

fn namespace_owner_upgrade(initialized: bool) {
    // The control uses the current producer; the regression uses the frozen
    // predecessor producer. Each has an isolated atomic index and restarted
    // Service, so no old in-memory authority or diagnostic mirror can rescue it.
    for legacy in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        let headers = HeaderMap::new();
        let (mut body, old_namespace, id, frozen) =
            seed_namespace(&service, &headers, true, initialized, legacy);
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
            .push(json!({"role":"user","content":"append after upgrade"}));
        assert_namespace_fences(&service, &headers, &body, &id, &frozen);
        let namespace = history::namespace(&service, &headers, &body).unwrap();
        let (selected, _pin) = history::resolve(&service, &namespace, &body).unwrap();
        assert_eq!(
            selected, id,
            "NAMESPACE_UPGRADE_OWNER: proven append ancestry must retain its paid/free quantity owner"
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
}

fn namespace_transcript_upgrade(shared: bool) {
    for legacy in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        let mut headers = HeaderMap::new();
        if !shared {
            headers.insert("x-carry-session", "upgrade-explicit".parse().unwrap());
        }
        let (body, old_namespace, id, frozen) =
            seed_namespace(&service, &headers, shared, false, legacy);
        let delta = json!({"role":"user","content":"resume after upgrade"});
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
        history::expand(&service, &namespace, &mut resumed).expect(
            "NAMESPACE_UPGRADE_TRANSCRIPT: a successfully observed identifier remains reachable across namespace upgrade");
        assert_eq!(resumed["input"], json!(expected));
        assert!(resumed.get("previous_response_id").is_none());
    }
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
        let (body, namespace, id, frozen) = seed_namespace(&service, &headers, true, false, false);
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

#[test]
fn namespace_upgrade_preserves_paid_history_owner() {
    namespace_owner_upgrade(false);
}

#[test]
fn namespace_upgrade_preserves_initialized_history_owner() {
    namespace_owner_upgrade(true);
}

#[test]
fn namespace_upgrade_preserves_implicit_observed_transcript() {
    namespace_transcript_upgrade(true);
}

#[test]
fn namespace_upgrade_preserves_explicit_observed_transcript() {
    namespace_transcript_upgrade(false);
}

#[test]
fn namespace_upgrade_capacity_rejection_preserves_complete_generation() {
    // A migration is not a retention request. Replacing a valid old generation
    // must not silently evict observed transcript proof just to fit its new root.
    // The roomy control also proves repeated preparation is a read-only no-op.
    for roomy in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        let headers = HeaderMap::new();
        let (body, old_namespace, id, frozen_money) =
            seed_namespace(&service, &headers, true, false, true);
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
        if roomy {
            let namespace = outcome.unwrap();
            assert_ne!(namespace, old_namespace);
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
        } else {
            let rejected = outcome.as_ref().err().map(|failure| failure.0)
                == Some(StatusCode::INSUFFICIENT_STORAGE);
            assert!(
                rejected && after == before,
                "NAMESPACE_UPGRADE_ATOMIC_CAPACITY: reject without replacing retained proof; status={:?}, unchanged={}",
                outcome.as_ref().err().map(|failure| failure.0),
                after == before
            );
        }
    }
}

// Exercise the production request/settlement path, not only index preparation.
// The predecessor is staged with its frozen producer; the separate actual CLI
// old -> new replay has no checkpoint edits and reaches the same boundary.
#[tokio::test]
async fn namespace_upgrade_alternating_ids_retains_explicit_conversation() {
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
    for legacy in [false, true] {
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
        let (body, _old_namespace, id, frozen) =
            seed_namespace(&service, &headers, false, false, legacy);
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
            "NAMESPACE_ALTERNATING_IDS: proven conversation must advance when the next response omits repeated metadata; legacy={legacy}"
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
}
