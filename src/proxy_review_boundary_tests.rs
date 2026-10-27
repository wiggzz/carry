// Actual render/dispatch and paid-owner boundaries; no parallel planner.
async fn review_capture_provider() -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<Value>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let router = Router::new().route("/review", post(move |axum::Json(body): axum::Json<Value>| {
        let sender = sender.clone();
        async move {
            sender.send(body).unwrap();
            axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"protected\":[],\"removable\":[],\"memories\":[]}"}]}],"usage":{"input_tokens":10000,"output_tokens":7,"input_tokens_details":{"cached_tokens":9000}}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/review", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, receiver, provider)
}

#[tokio::test]
async fn prospective_review_affinity_equals_actual_dispatch_after_restart_and_key_change() {
    let (url, mut captures, provider) = review_capture_provider().await;
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.classifier_url = Some(url);
    let mut session = semantic_subject();
    for affinity in [None, Some("caller-affinity"), Some("changed-affinity")] {
        session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
        session.review_context = json!({"model":"gpt-6-luna"});
        if let Some(key) = affinity {
            session.review_context["prompt_cache_key"] = json!(key);
        }
        let projection = planning::Projection::canonical(&session);
        let prospective = review_body_projected(&service.config, &session, &projection);
        let mut run = ReviewRun::default();
        assert!(run_review(&service, "affinity", &mut session, &projection, &mut run).await);
        let dispatched = tokio::time::timeout(Duration::from_secs(10), captures.recv())
            .await
            .expect("reviewer transport capture deadline")
            .unwrap();
        assert_eq!(
            prospective, dispatched,
            "fit/pricing must use the actual affinity body"
        );
        assert_eq!(
            dispatched["prompt_cache_key"],
            affinity.unwrap_or("carry-review")
        );
    }
    provider.abort();
}

#[tokio::test]
async fn completed_review_waits_for_durable_paid_owner_before_publishing_captured_h() {
    let (url, mut captures, provider) = review_capture_provider().await;
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.classifier_url = Some(url);
    let service = Arc::new(service);
    let id = "paid-owner";
    let mut session = semantic_subject();
    session.history_shared = true;
    session.bootstrap_review_pending = true;
    let h = session.render_primary();
    let owner = session_lock(&service, id).await;
    let (waiter, mut waits) = tokio::sync::mpsc::unbounded_channel();
    *service.lock_waits.lock().unwrap() = Some(waiter);
    let projection = planning::Projection::canonical(&session);
    dispatch_review_for(
        service.clone(),
        id.into(),
        &mut session,
        projection,
        "paid-boundary".into(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), captures.recv())
        .await
        .expect("reviewer transport capture deadline")
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), waits.recv())
            .await
            .unwrap()
            .unwrap(),
        id
    );
    assert!(
        history::advice(&service, "fixture-owner", &h)
            .unwrap()
            .is_none(),
        "completed H cannot be reusable while its known receipt is still only in a blocked worker"
    );
    let durable = history::restore(&service, id).unwrap();
    assert_eq!(durable.shadow.calls, 0);
    assert!(durable.background_review_pending);
    // Advance the paid owner while holding its lock; publication must still use H.
    observe_source(&mut session, "advanced current H");
    save(&service, id, &session).unwrap();
    drop(owner);
    tokio::time::timeout(Duration::from_secs(3), async {
        while service.background_reviews.lock().await.contains(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let durable = history::restore(&service, id).unwrap();
    assert_eq!(durable.shadow.calls, 1);
    assert_eq!(durable.shadow.input_tokens, 10000);
    assert_eq!(durable.shadow.output_tokens, 7);
    assert!(!durable.background_review_pending);
    assert!(
        history::advice(&service, "fixture-owner", &h)
            .unwrap()
            .is_some()
    );
    assert_eq!(durable.history.len(), h.len() + 1);
    provider.abort();
}

#[test]
fn adapted_wire_cache_prefix_protects_cached_native_members_not_canonical_positions() {
    let mut config = reviewer_config();
    config.codex_login = true;
    config.classifier_cache_policy = ClassifierCachePolicy::Disabled;
    let mut model = resolve_model(
        "gpt-6-luna",
        Route::new("https://api.openai.com/v1/responses", true),
        None,
        None,
        2048,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    // Fixture-only capability, not a provider entitlement claim.
    model.cache = carry::models::model_spec("gpt-6-luna").unwrap().cache;
    config.model_resolution = Some(ResolvedPair {
        schema_version: 1,
        primary: model.clone(),
        reviewer: model.clone(),
    });
    let mut session = Session::default();
    let h = vec![
        json!({"role":"system","content":"fixed direction"}),
        json!({"role":"user","content":"goal"}),
        json!({"role":"assistant","content":"cached result ".repeat(3000)}),
        json!({"role":"user","content":"separator"}),
        json!({"role":"assistant","content":"cold disposable suffix ".repeat(3000)}),
        json!({"role":"user","content":"task"}),
    ];
    session.ingest(&h).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":[3,5],"memories":[]}),
        )
        .unwrap();
    let body = json!({"model":"gpt-6-luna","prompt_cache_key":"stable","input":h});
    let mut cached_body = body.clone();
    cached_body["input"] = json!(&h[..3]);
    let cached = primary_wire_body(&config, &cached_body);
    assert_eq!(cached["input"].as_array().unwrap(), &h[1..3]);
    crate::proxy_cache::observe(
        &mut session.upstream_cache,
        &cached,
        &json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":9000}}),
        Some(&model),
        &config.primary_cache_auth,
        now(),
    );
    assert_eq!(session.upstream_cache.len(), 1);
    assert_eq!(session.upstream_cache[0].input_len, 2);
    let (_, report) = plan(&config, &session, &body);
    assert!(
        report["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["omitted_members"] == json!([5])),
        "wire user+assistant prefix must protect canonical assistant 3 while permitting suffix 5: {report}"
    );
}
