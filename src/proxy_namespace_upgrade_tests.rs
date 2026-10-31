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
        // Genuine model/owner/prompt boundaries must not borrow legacy ancestry.
        for boundary in ["model", "instructions"] {
            let mut different = body.clone();
            different[boundary] = json!("different-boundary");
            let namespace = history::namespace(&service, &headers, &different).unwrap();
            let (selected, pin) = history::resolve(&service, &namespace, &different).unwrap();
            assert_ne!(selected, id);
            drop(pin);
        }
        let mut other_owner = headers.clone();
        other_owner.insert("x-carry-tenant", "another-owner".parse().unwrap());
        let other_namespace = history::namespace(&service, &other_owner, &body).unwrap();
        let (other, pin) = history::resolve(&service, &other_namespace, &body).unwrap();
        assert_ne!(other, id);
        drop(pin);
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
        let namespace = history::namespace(&service, &headers, &resumed).unwrap();
        history::expand(&service, &namespace, &mut resumed).expect(
            "NAMESPACE_UPGRADE_TRANSCRIPT: a successfully observed identifier remains reachable across namespace upgrade");
        assert_eq!(resumed["input"], json!(expected));
        assert!(resumed.get("previous_response_id").is_none());
    }
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
