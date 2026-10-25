// Synthetic loopback receipts execute the real primary commit/cache path.
async fn catalog_cache_receipt() -> (ProxyCli, Session, Value) {
    let router = Router::new().route("/primary", post(|| async {
        axum::Json(json!({"id":"catalog-cache","status":"completed","output":[],"usage":{"input_tokens":10000,"output_tokens":1,"input_tokens_details":{"cached_tokens":9000}}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.upstream_url = format!("http://{address}/primary");
    service.config.classifier_cache_policy = ClassifierCachePolicy::Disabled;
    service.config.review_min_primary_tokens = 1000000;
    service.config.review_every_requests = 100;
    service.config.payoff_requests = 1;
    let service = Arc::new(service);
    let mut body = json!({"model":"gpt-6-sol","input":[
        {"role":"user","content":"retained goal ".repeat(800)},
        {"role":"assistant","content":"old disposable result ".repeat(300)},
        {"role":"user","content":"middle task"}
    ]});
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "catalog-cache".parse().unwrap());
    let response = forward_request(
        service.clone(),
        headers,
        Bytes::from(serde_json::to_vec(&body).unwrap()),
        false,
        "cache-receipt".into(),
    )
    .await
    .unwrap();
    assert!(response.status().is_success());
    axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let mut session = load(&service, &{
        let mut h = HeaderMap::new();
        h.insert("x-carry-session", "catalog-cache".parse().unwrap());
        identity(&h).unwrap().unwrap()
    })
    .unwrap();
    assert_eq!(session.primary.calls, 1);
    assert_eq!(session.primary.cached_tokens, 9000);
    session.upstream_cache = history::cache(&service).unwrap();
    assert_eq!(session.upstream_cache.len(), 1);
    body["input"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","content":"new disposable result ".repeat(300)}),
        json!({"role":"user","content":"current task"}),
    ]);
    session.ingest(body["input"].as_array().unwrap()).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    let groups = session.groups();
    session
        .apply_advice(
            &groups,
            &json!({"protected":["g1"],"removable":["g2","g4"],"memories":[]}),
        )
        .unwrap();
    // The gateway has no forecast capability. A deliberately enabled synthetic
    // snapshot lets the positive control exercise receipt namespace, not vendors.
    session.model_resolution.as_mut().unwrap().primary.cache = resolve_model(
        "gpt-6-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        16384,
        10,
    )
    .unwrap()
    .cache;
    let record = semantic::Record::capture(&session);
    let mut restored: Session =
        serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
    record.apply(&mut restored);
    restored.upstream_cache = history::cache(&service).unwrap();
    assert_eq!(restored.primary.cost_usd, session.primary.cost_usd);
    provider.abort();
    (service.config.clone(), restored, body)
}

#[tokio::test]
async fn catalog_r1_receipt_credit_is_bound_to_route_auth_class_and_restart() {
    let (_config, session, body) = catalog_cache_receipt().await;
    let exact = &session.model_resolution.as_ref().unwrap().primary;
    let cost = |cache: &[CacheEvidence], model: &ResolvedModel| {
        resolved_view_costs(&body, &body, cache, "gpt-6-sol", 1, Some(model), "")
            .unwrap()
            .keep
    };
    assert!(
        cost(&session.upstream_cache, exact) < cost(&[], exact),
        "exact-route receipt survives restart"
    );
    for route in [
        Route::new("https://api.openai.com/v1/responses", false),
        Route::new("https://other-gateway.invalid/v1/responses", false),
        Route::new("https://other-gateway.invalid/other/responses", false),
        Route::new("https://api.openai.com/v1/responses", true),
    ] {
        let mut changed = exact.clone();
        changed.route = route;
        assert_eq!(
            cost(&session.upstream_cache, &changed),
            cost(&[], &changed),
            "CATALOG-R1: another destination/auth namespace must start cold"
        );
    }
    let mut changed = exact.clone();
    changed.route.processing_tier = "priority".into();
    assert_eq!(cost(&session.upstream_cache, &changed), cost(&[], &changed));
}

#[tokio::test]
async fn catalog_r1_cache_preserving_candidates_require_the_exact_receipt_namespace() {
    let (mut config, mut session, body) = catalog_cache_receipt().await;
    config.model_resolution = session.model_resolution.clone();
    let mut cold = session.clone();
    cold.upstream_cache.clear();
    let (_, exact) = plan(&config, &session, &body);
    let (_, empty) = plan(&config, &cold, &body);
    assert!(
        exact["candidates"].as_array().unwrap().len()
            > empty["candidates"].as_array().unwrap().len(),
        "exact-route positive receipt must enumerate a distinct preserved-prefix candidate"
    );
    let paid = session.primary.cost_usd;
    session.model_resolution.as_mut().unwrap().primary.route =
        Route::new("https://api.openai.com/v1/responses", false);
    cold.model_resolution = session.model_resolution.clone();
    let (_, changed) = plan(&config, &session, &body);
    let (_, control) = plan(&config, &cold, &body);
    assert_eq!(
        changed, control,
        "CATALOG-R1: incompatible receipt must affect neither candidates nor economics"
    );
    assert_eq!(session.primary.cost_usd, paid, "no retrospective repricing");
}
