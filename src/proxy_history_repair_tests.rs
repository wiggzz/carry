// Private production-ingress seams, actual loopback primary, deterministic lock
// ordering. No alternate resolver/planner and no source-wording assertions.
async fn history_race_send(service: Arc<Service>, input: Value) {
    let response = forward_request(
        service,
        HeaderMap::new(),
        Bytes::from(serde_json::to_vec(&json!({"model":"gpt-6-luna","input":input})).unwrap()),
        false,
        logging::request_id(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
}

fn history_race_owner(service: &Service, marker: &str) -> String {
    let store: Value = serde_json::from_slice(
        &std::fs::read(service.config.state_dir.join("history.index")).unwrap(),
    )
    .unwrap();
    store["snapshots"]
        .as_object()
        .unwrap()
        .iter()
        .find_map(|(id, value)| (value["title"] == marker).then(|| id.clone()))
        .unwrap()
}

#[tokio::test]
async fn history_two_advanced_lock_selections_never_reset_sibling() {
    tokio::time::timeout(Duration::from_secs(15), history_two_tip_interleaving())
        .await
        .expect("controlled two-lock interleaving must finish");
}

async fn history_two_tip_interleaving() {
    use std::sync::atomic::AtomicUsize;
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let seen = seen.clone();
            async move {
                axum::Json(
                    json!({"id":format!("race_{}",seen.fetch_add(1, Ordering::SeqCst)),
            "status":"completed","output":[],"usage":{"input_tokens":100,"output_tokens":1}}),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut service = checkpoint_service(directory.path());
    service.config.upstream_url = endpoint;
    service.config.review_every_requests = 100;
    service.config.bootstrap_min_input_bytes = usize::MAX;
    let service = Arc::new(service);
    let a = json!({"role":"user","content":"race common goal"});
    let b = json!({"role":"assistant","content":"race B"});
    let x = json!({"role":"user","content":"first winner X"});
    let y = json!({"role":"user","content":"second winner Y"});
    let c = json!({"role":"user","content":"waiting divergent C"});
    history_race_send(service.clone(), json!([a, b])).await;
    let first = history_race_owner(&service, "race common goal");
    // Explicit selected-state fixture: primary receipts came from ingress;
    // nonempty removal/memory authority must survive the lock race as well.
    let mut selected = load(&service, &first).unwrap();
    selected.history[1].removed = true;
    selected.memories.push(crate::proxy_state::Memory {
        id: 99,
        source_ids: vec![selected.history[1].id],
        text: "first branch selected fact".into(),
    });
    save(&service, &first, &selected).unwrap();
    let first_lock = session_lock(&service, &first).await;
    let (sender, mut waits) = tokio::sync::mpsc::unbounded_channel();
    *service.lock_waits.lock().unwrap() = Some(sender);
    let winner_one = tokio::spawn(history_race_send(service.clone(), json!([a, b, x])));
    assert_eq!(waits.recv().await.unwrap(), first);
    let waiter = tokio::spawn(history_race_send(service.clone(), json!([a, b, c])));
    assert_eq!(waits.recv().await.unwrap(), first);
    // While both requests wait on the first live tip, create a shorter live tip.
    history_race_send(service.clone(), json!([a])).await;
    let second = waits.recv().await.unwrap();
    assert_ne!(second, first);
    let second_lock = session_lock(&service, &second).await;
    assert_eq!(waits.recv().await.unwrap(), second);
    let winner_two = tokio::spawn(history_race_send(service.clone(), json!([a, y])));
    assert_eq!(waits.recv().await.unwrap(), second);
    drop(first_lock);
    winner_one.await.unwrap();
    // Waiter has re-resolved and actually queued behind the second winner.
    assert_eq!(waits.recv().await.unwrap(), second);
    let first_bytes = std::fs::read(directory.path().join(format!("{first}.json"))).unwrap();
    drop(second_lock);
    winner_two.await.unwrap();
    waiter.await.unwrap();
    *service.lock_waits.lock().unwrap() = None;
    assert_eq!(
        std::fs::read(directory.path().join(format!("{first}.json"))).unwrap(),
        first_bytes
    );
    let saved_first = load(&service, &first).unwrap();
    assert!(saved_first.history[1].removed);
    assert_eq!(saved_first.memories[0].text, "first branch selected fact");
    assert_eq!(saved_first.completed_requests, 2);
    assert_eq!(saved_first.primary.calls, 2);
    let saved_second = load(&service, &second).unwrap();
    assert_eq!(
        saved_second
            .history
            .iter()
            .map(|i| i.value.clone())
            .collect::<Vec<_>>(),
        vec![a.clone(), y.clone()],
        "second selected lock also advanced: re-resolve again, never implicitly reset it"
    );
    assert_eq!(saved_second.completed_requests, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    // Same-history retry stays on its selected branch; an unmatched history is fresh.
    history_race_send(service.clone(), json!([a, y])).await;
    assert_eq!(load(&service, &second).unwrap().completed_requests, 3);
    history_race_send(
        service.clone(),
        json!([{"role":"user","content":"genuinely unmatched"}]),
    )
    .await;
    assert_eq!(load(&service, &second).unwrap().completed_requests, 3);
    // New service instance exercises durable readback, not a cached Session.
    let restarted = checkpoint_service(directory.path());
    assert_eq!(load(&restarted, &second).unwrap().completed_requests, 3);
    assert_eq!(
        std::fs::read(directory.path().join(format!("{first}.json"))).unwrap(),
        first_bytes
    );
    provider.abort();
}

#[tokio::test]
async fn history_failed_small_publication_and_lowered_restart_preserve_generation() {
    let directory = tempfile::tempdir().unwrap();
    let app = Router::new().route(
        "/v1/responses",
        post(|| async {
            axum::Json(
                json!({"id":"durable_before_capacity", "status":"completed", "output":[],
            "usage":{"input_tokens":100,"output_tokens":1}}),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut service = checkpoint_service(directory.path());
    service.config.upstream_url = endpoint;
    service.config.review_every_requests = 100;
    service.config.bootstrap_min_input_bytes = usize::MAX;
    let service = Arc::new(service);
    let input = json!([{"role":"user","content":"durable before capacity"}]);
    history_race_send(service.clone(), input.clone()).await;
    let owner = history_race_owner(&service, "durable before capacity");
    let index = directory.path().join("history.index");
    let prior = std::fs::read(&index).unwrap();
    let selected = load(&service, &owner).unwrap();
    // The old generation itself fits, but a new selected-owner publication
    // does not. Pinning must preserve the old bytes instead of evicting owner.
    let mut bounded = (*service).clone();
    bounded.config.history_max_bytes = prior.len().try_into().unwrap();
    let pin = history::pin(&bounded, &owner).unwrap();
    let mut oversized = selected.clone();
    oversized.title = Some("new oversized selected metadata ".repeat(prior.len()));
    assert!(history::committed(&bounded, &owner, &oversized, None).is_err());
    assert_eq!(std::fs::read(&index).unwrap(), prior);
    assert_eq!(load(&bounded, &owner).unwrap().primary.calls, 1);
    drop(pin);
    let mut lowered = (*service).clone();
    lowered.config.history_max_bytes = 67;
    let pin = history::pin(&lowered, &owner).unwrap();
    assert!(history::committed(&lowered, &owner, &selected, None).is_err());
    assert_eq!(
        std::fs::read(&index).unwrap(),
        prior,
        "failed publication cannot replace prior generation"
    );
    assert!(!directory.path().join(".history-index.tmp").exists());
    drop(pin);
    // A lowered-budget process must fail closed on an oversized previous
    // generation, not pretend it was absent or replace it with an empty index.
    let lowered = Arc::new(lowered);
    for _ in 0..2 {
        let result = forward_request(
            lowered.clone(),
            HeaderMap::new(),
            Bytes::from(serde_json::to_vec(&json!({"model":"gpt-6-luna","input":input})).unwrap()),
            false,
            logging::request_id(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(std::fs::read(&index).unwrap(), prior);
    }
    let restarted = checkpoint_service(directory.path());
    assert_eq!(load(&restarted, &owner).unwrap().primary.calls, 1);
    assert_eq!(std::fs::read(index).unwrap(), prior);
    provider.abort();
}
