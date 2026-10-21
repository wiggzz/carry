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
