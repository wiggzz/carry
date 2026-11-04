// Credential-free production forward/relay fixture; no provider calls.
#[tokio::test]
async fn phase_timing_separates_local_transport_body_and_settlement_without_wire_changes() {
    let directory = tempfile::tempdir().unwrap();
    let (captured_tx, mut captured_rx) = mpsc::unbounded_channel();
    let expected = json!({"id":"fixture-response","status":"completed","output":[],
        "usage":{"input_tokens":100,"output_tokens":5,"input_tokens_details":{"cached_tokens":0}}})
    .to_string();
    let reply = expected.clone();
    let router = Router::new().route(
        "/responses",
        post(move |bytes: Bytes| {
            let tx = captured_tx.clone();
            let reply = reply.clone();
            async move {
                tx.send(bytes).unwrap();
                tokio::time::sleep(Duration::from_millis(60)).await;
                let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(2);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    tx.send(Ok(Bytes::from(reply))).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(60)).await;
                });
                (
                    [("content-type", "application/json")],
                    Body::from_stream(ReceiverStream::new(rx)),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut service = checkpoint_service(directory.path());
    service.config.mode = Mode::Off;
    service.config.upstream_url = format!("http://{addr}/responses");
    let (wait_tx, mut wait_rx) = mpsc::unbounded_channel();
    *service.lock_waits.lock().unwrap() = Some(wait_tx);
    let service = Arc::new(service);
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "PRIVATE_SESSION".parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    let lock = session_lock(&service, &id).await;
    wait_rx.recv().await.unwrap();
    let body = Bytes::from_static(br#"{"model":"gpt-6-luna","input":[{"role":"user","content":"PRIVATE_PROMPT"}],"stream":false}"#);
    let sent = body.clone();
    let forward_service = service.clone();
    let task = tokio::spawn(async move {
        forward_request(
            forward_service,
            headers,
            body,
            false,
            "timing-fixture".into(),
        )
        .await
    });
    wait_rx.recv().await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    drop(lock);
    let response = task.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        captured_rx.recv().await.unwrap(),
        sent,
        "complete native wire is unchanged"
    );
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
        expected
    );
    let saved = load(&service, &id).unwrap();
    assert_eq!(saved.primary.calls, 1);
    assert_eq!(saved.primary.input_tokens, 100);
    assert_eq!(saved.primary.output_tokens, 5);
    assert_eq!(saved.shadow.calls, 0);
    let events = pr154_journal(directory.path(), &id);
    let timings: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "phase_timing")
        .collect();
    assert_eq!(
        timings.len(),
        1,
        "one correlated terminal phase summary is missing"
    );
    let t = &timings[0]["data"];
    assert_eq!(timings[0]["request_id"], "timing-fixture");
    assert_eq!(t["role"], "primary");
    assert_eq!(t["outcome"], "complete");
    assert_eq!(t["incomplete"], false);
    assert!(t["primary_lock_wait_us"].as_u64().unwrap() >= 30_000);
    let dispatch = t["attempts"][0]["dispatch_us"].as_u64().unwrap();
    let headers = t["attempts"][0]["headers_us"].as_u64().unwrap();
    let first = t["first_body_us"].as_u64().unwrap();
    let end = t["body_complete_us"].as_u64().unwrap();
    let finish = t["total_us"].as_u64().unwrap();
    assert!(dispatch >= 30_000);
    assert!(headers - dispatch >= 30_000);
    assert!(first - headers >= 30_000);
    assert!(end - first >= 30_000);
    assert!(finish >= end);
    assert_eq!(t["blocking_review_us"], Value::Null);
    assert!(t["settlement_us"].as_u64().is_some());
    assert!(t["native_attempt_id"].as_str().is_some());
    assert!(
        !serde_json::to_string(&timings)
            .unwrap()
            .contains("PRIVATE_")
    );
    server.abort();
}
