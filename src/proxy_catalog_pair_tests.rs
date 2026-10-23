// Included in proxy::tests: actual metadata, primary transport, held review,
// saved-pair reuse, model-triggered pair refresh and asynchronous settlement.
fn catalog_pair_service(directory: &Path, home: &Path, endpoint: &str) -> Service {
    let mut service = checkpoint_service(directory);
    service.config.upstream_url = endpoint.into();
    service.config.codex_login = true;
    service.config.codex_home = Some(home.to_path_buf());
    service.config.classifier_cache_policy = ClassifierCachePolicy::Disabled;
    service.config.classifier_max_output_tokens = 64;
    service.config.context_margin_percent = 0;
    service.config.background_reviews = true;
    service.config.review_every_requests = 1;
    service.config.review_min_primary_tokens = 0;
    service.config.review_min_new_tokens = 0;
    service.config.bootstrap_min_input_bytes = usize::MAX;
    service
}

async fn catalog_pair_send(service: Arc<Service>, body: &Value, request: &str) {
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "catalog-pair".parse().unwrap());
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        forward_request(
            service,
            headers,
            Bytes::from(serde_json::to_vec(body).unwrap()),
            false,
            request.into(),
        ),
    )
    .await
    .expect("fitting primary must complete before held reviewer release")
    .unwrap();
    assert!(response.status().is_success());
    axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
}

async fn catalog_pair_refresh_control(kind: &str) {
    catalog_pair_lease_control(kind, false).await;
}

async fn catalog_pair_lease_control(kind: &str, lease: bool) {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::sync::atomic::{AtomicU32, AtomicUsize};
    let directory = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let token = format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(
            r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}}"#
        )
    );
    std::fs::write(home.path().join("auth.json"), serde_json::to_vec(&json!({
        "version":1,"access_token":token,"refresh_token":"synthetic-refresh","expires_at_ms":4102444800000u64
    })).unwrap()).unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let capacity = Arc::new(AtomicU32::new(40000));
    let metadata_calls = Arc::new(AtomicUsize::new(0));
    let review_calls = Arc::new(AtomicUsize::new(0));
    let review_wire = Arc::new(Mutex::new(Value::Null));
    let long_memory = "retained learning ".repeat(200);
    let advice = json!({"protected":[],"removable":[],"memories":[{
        "source_ids":["g2"],"text":if kind == "grow" {"short retained fact"} else {&long_memory}
    }]});
    let (signal, gate, rc, captured, response_advice) = (
        started.clone(),
        release.clone(),
        review_calls.clone(),
        review_wire.clone(),
        advice.clone(),
    );
    let (advertised, mc) = (capacity.clone(), metadata_calls.clone());
    let router = Router::new()
        .route("/v1/models", get(move || { let (advertised,mc) = (advertised.clone(),mc.clone()); async move {
            mc.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"models":[
                {"slug":"gpt-6-sol","visibility":"list","context_window":100000},
                {"slug":"gpt-6-astra","visibility":"list","context_window":100000},
                {"slug":"gpt-6-luna","visibility":"list","context_window":advertised.load(Ordering::SeqCst)}
            ]}))
        }}))
        .route("/v1/responses", post(move |axum::Json(body): axum::Json<Value>| {
            let (signal, gate, rc, captured, advice) =
                (signal.clone(),gate.clone(),rc.clone(),captured.clone(),response_advice.clone());
            async move {
                let reviewer = body["model"] == "gpt-6-luna";
                let output = if reviewer {
                    assert_eq!(rc.fetch_add(1,Ordering::SeqCst), 0, "exactly one held review");
                    *captured.lock().await = body.clone();
                    signal.notify_one();
                    gate.notified().await;
                    json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}])
                } else { json!([]) };
                let response = json!({"id":"catalog-pair-response","status":"completed","output":output,
                    "usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":40}}});
                ([ ("content-type","text/event-stream") ],format!("data: {}\n\n",json!({"type":"response.completed","response":response})))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut body = json!({"model":"gpt-6-sol","input":[
        {"role":"user","content":"retained goal"},
        {"role":"assistant","content":"completed native source"},
        {"role":"user","content":"current task"}
    ],"max_output_tokens":10});
    let service = catalog_pair_service(directory.path(), home.path(), &endpoint);
    // Derive the small token budget from the real aggregate memory rendering,
    // not a prompt string assertion or an invented per-fact token count.
    let mut probe = Session::default();
    probe.ingest(body["input"].as_array().unwrap()).unwrap();
    for item in &mut probe.history {
        item.exposed = true;
    }
    probe.observe_groups(&probe.groups());
    probe.review_context = cache_base(&body);
    let base_tokens = estimate(&memory_review_body(&service.config, &probe));
    let mut with_memory = probe.clone();
    with_memory.apply_advice(&probe.groups(), &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":long_memory}]})).unwrap();
    assert_eq!(with_memory.memories.len(), 1);
    let memory_tokens = estimate(&memory_review_body(&service.config, &with_memory));
    let small = (2.0 * memory_tokens - base_tokens).ceil() as u32 - 64;
    let large = small + 20000;
    let (old_capacity, new_capacity) = match kind {
        "shrink" => (large, small),
        "grow" => (small, large),
        "unchanged" => (large, large),
        _ => unreachable!(),
    };
    capacity.store(old_capacity, Ordering::SeqCst);
    let service = Arc::new(service);
    catalog_pair_send(service.clone(), &body, "seed").await;
    let id = pr154_session_id("catalog-pair");
    let mut seeded = load(&service, &id).unwrap();
    assert_eq!(metadata_calls.load(Ordering::SeqCst), 1);
    assert_eq!(review_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        seeded
            .model_resolution
            .as_ref()
            .unwrap()
            .reviewer
            .capacity_tokens,
        Some(old_capacity)
    );
    // Synthetic historical tariff fixture, installed in memory before normal
    // save. No production checkpoint is read or edited by this regression.
    let old_rates = Rates {
        input: 7.0,
        cached: 0.7,
        write: 9.0,
        output: 21.0,
    };
    seeded.model_resolution.as_mut().unwrap().reviewer.rates = Some(old_rates);
    seeded
        .model_resolution
        .as_mut()
        .unwrap()
        .reviewer
        .price_basis = "synthetic_fixture_only".into();
    if lease {
        // Stage maturity only; accept protection through the production callback.
        seeded.initialize_keep_leases(8);
        seeded
            .apply_advice(
                &seeded.groups(),
                &json!({"protected":["g2"],"removable":[],"memories":[]}),
            )
            .unwrap();
        seeded.completed_requests += 8;
    }
    save(&service, &id, &seeded).unwrap();
    drop(service);
    capacity.store(new_capacity, Ordering::SeqCst);
    // New process-equivalent service: empty metadata OnceCell, same config/home
    // and real persisted session; no old in-flight task or registry is retained.
    let service = Arc::new(catalog_pair_service(
        directory.path(),
        home.path(),
        &endpoint,
    ));
    catalog_pair_send(service.clone(), &body, "old-pair-dispatch").await;
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    let before = load(&service, &id).unwrap();
    assert!(before.background_review_pending);
    if lease {
        assert!(
            before
                .keep_leases
                .as_ref()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .targets
                .contains_key("g2"),
            "actual held dispatch owns due g2"
        );
    }
    assert_eq!(before.shadow.calls, 0);
    assert_eq!(
        metadata_calls.load(Ordering::SeqCst),
        1,
        "same saved pair must not refresh after restart"
    );
    assert_eq!(
        before
            .model_resolution
            .as_ref()
            .unwrap()
            .reviewer
            .capacity_tokens,
        Some(old_capacity)
    );
    body["model"] = json!("gpt-6-astra");
    body["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"append-compatible newest task"}));
    catalog_pair_send(service.clone(), &body, "current-pair-refresh").await;
    let current = load(&service, &id).unwrap();
    let pair = current.model_resolution.as_ref().unwrap();
    assert_eq!(pair.primary.model, "gpt-6-astra");
    assert_eq!(pair.reviewer.capacity_tokens, Some(new_capacity));
    assert_eq!(
        metadata_calls.load(Ordering::SeqCst),
        2,
        "model change must actually resolve the whole pair"
    );
    assert_eq!(current.review_epoch, before.review_epoch);
    assert_eq!(current.history_rebases, before.history_rebases);
    assert_eq!(current.reviewer_compactions, before.reviewer_compactions);
    assert!(current.background_review_pending);
    assert_eq!(current.primary.calls, 3);
    assert_eq!(current.shadow.calls, 0);
    let mut current_config = service.config.clone();
    current_config.classifier_max_input_tokens = new_capacity;
    current_config.model_resolution = Some(pair.clone());
    assert!(
        review_fits(&current_config, &review_body(&current_config, &current)),
        "native review fits; not capacity-wait/setup RED"
    );
    let mut candidate = current.clone();
    candidate.apply_advice(&current.groups(), &advice).unwrap();
    assert_eq!(candidate.memories.len(), 1);
    assert_eq!(
        memory_admission_fits(&current_config, &candidate),
        kind != "shrink"
    );
    let mut old_config = current_config.clone();
    old_config.classifier_max_input_tokens = old_capacity;
    assert!(
        memory_admission_fits(&old_config, &candidate),
        "memory must be accepted by the old dispatched budget"
    );
    let paid_primary = current.primary.cost_usd;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.background_reviews.lock().await.contains(&id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let merged = load(&service, &id).unwrap();
    assert_eq!(merged.shadow.calls, 1);
    assert_eq!(merged.shadow.unavailable_cost_calls, 0);
    assert!(
        (merged.shadow.cost_usd - (60.0 * 7.0 + 40.0 * 0.7 + 10.0 * 21.0) / 1e6).abs() < 1e-12,
        "paid receipt uses OLD frozen tariff"
    );
    let input = estimate(&review_wire.lock().await.clone());
    assert!(
        (merged
            .cost_trajectory
            .as_ref()
            .unwrap()
            .snapshot()
            .reviewer_cost
            - (input * 7.0 + 10.0 * 21.0) / 1e6)
            .abs()
            < 1e-12
    );
    assert_eq!(merged.primary.cost_usd, paid_primary);
    assert_eq!(merged.completed_requests, if lease { 11 } else { 3 });
    if lease {
        assert!(merged.keep_leases.as_ref().unwrap().pending.is_none());
        assert_eq!(
            merged.opinions.get(&2).map(String::as_str),
            if kind == "shrink" { Some("keep") } else { None },
            "CURRENT catalog rejection cannot release the held target; accepted omission can"
        );
        assert_eq!(merged.released_group(&merged.groups()[1]), kind != "shrink");
    }
    assert!(!merged.background_review_pending);
    assert_eq!(
        serde_json::to_value(&merged.model_resolution).unwrap(),
        serde_json::to_value(&current.model_resolution).unwrap(),
        "never replace current contract with dispatch contract"
    );
    let entries = pr154_journal(directory.path(), &id);
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["event"] == "background_review_merged")
            .count(),
        1,
        "advice lineage stays compatible"
    );
    assert_eq!(
        merged.memories.len(),
        usize::from(kind != "shrink"),
        "CATALOG-R2: delayed memory admission belongs to CURRENT saved pair"
    );
    assert_eq!(
        merged.review_feedback["rejected_count"],
        if kind == "shrink" { 1 } else { 0 }
    );
    drop(service);
    let restarted = catalog_pair_service(directory.path(), home.path(), &endpoint);
    let restored = load(&restarted, &id).unwrap();
    assert_eq!(
        restored.shadow.calls, 1,
        "restart must not settle receipt twice"
    );
    assert_eq!(restored.shadow.cost_usd, merged.shadow.cost_usd);
    if lease {
        assert_eq!(
            serde_json::to_value(&restored.keep_leases).unwrap(),
            serde_json::to_value(&merged.keep_leases).unwrap()
        );
        assert_eq!(restored.opinions, merged.opinions);
    }
    assert_eq!(restored.memories.len(), merged.memories.len());
    assert_eq!(review_calls.load(Ordering::SeqCst), 1);
    provider.abort();
}

#[tokio::test]
async fn catalog_r2_held_review_rejects_old_large_memory_after_current_pair_shrinks() {
    catalog_pair_refresh_control("shrink").await;
}
#[tokio::test]
async fn catalog_r2_held_review_accepts_fitting_memory_after_current_pair_grows() {
    catalog_pair_refresh_control("grow").await;
}
#[tokio::test]
async fn catalog_r2_held_review_preserves_unchanged_capacity_positive_control() {
    catalog_pair_refresh_control("unchanged").await;
}

#[tokio::test]
async fn lease_join_catalog_rejection_preserves_held_target_and_settles_old_tariff_once() {
    catalog_pair_lease_control("shrink", true).await;
}

#[tokio::test]
async fn lease_join_catalog_acceptance_releases_held_target_positive_control() {
    catalog_pair_lease_control("grow", true).await;
}
