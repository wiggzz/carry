// Real held loopback transport, cancelled caller future, durable settlement.

#[tokio::test]
async fn cancelled_preheader_caller_does_not_leave_an_unresolved_attempt() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let arrived = Arc::new(tokio::sync::Notify::new());
    let headers_ready = Arc::new(tokio::sync::Notify::new());
    let body_ready = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let router = Router::new().route("/primary", post({
        let arrived = arrived.clone();
        let headers_ready = headers_ready.clone();
        let body_ready = body_ready.clone();
        let calls = calls.clone();
        move || {
            let arrived = arrived.clone();
            let headers_ready = headers_ready.clone();
            let body_ready = body_ready.clone();
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                let response = json!({"id":format!("cancel-{n}"),"status":"completed","output":[],
                    "usage":{"input_tokens":1000,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}});
                if n != 2 {
                    return axum::Json(response).into_response();
                }
                arrived.notify_one();
                headers_ready.notified().await;
                let (sender, receiver) = mpsc::channel(1);
                tokio::spawn(async move {
                    body_ready.notified().await;
                    let _ = sender.send(Ok::<_, std::io::Error>(Bytes::from(
                        serde_json::to_vec(&response).unwrap(),
                    ))).await;
                });
                Response::builder()
                    .header("content-type", "application/json")
                    .body(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver)))
                    .unwrap()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.upstream_url = format!("http://{address}/primary");
    service.config.review_min_primary_tokens = 1000000;
    service.config.review_every_requests = 100;
    service.config.bootstrap_min_input_bytes = 8388608;
    let service = Arc::new(service);
    let body = Bytes::from(
        serde_json::to_vec(&json!({"model":"gpt-6-luna", "input":[
            {"role":"user", "content":"Cancellation-safe accounting fixture"}
        ]}))
        .unwrap(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "preheader-cancellation".parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    let first = forward_request(
        service.clone(),
        headers.clone(),
        body.clone(),
        false,
        "cancel-seed".into(),
    )
    .await
    .unwrap();
    axum::body::to_bytes(first.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let seed = load(&service, &id).unwrap();
    assert_eq!(seed.primary.calls, 1);
    assert!(!seed.primary_pending);
    let caller = tokio::spawn(forward_request(
        service.clone(),
        headers.clone(),
        body.clone(),
        false,
        "cancel-held".into(),
    ));
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    let pending = load(&service, &id).unwrap();
    assert!(
        pending.primary_pending,
        "cancel only after durable attempt reservation and upstream arrival"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    headers_ready.notify_one();
    let deadline = Instant::now() + Duration::from_secs(2);
    let settled = loop {
        let session = load(&service, &id).unwrap();
        if !session.primary_pending || Instant::now() >= deadline {
            break session;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    // Release/abort owned provider resources before grading even the RED arm.
    body_ready.notify_one();
    assert!(
        !settled.primary_pending,
        "CANCEL: caller cancellation before headers must still settle the reserved attempt"
    );
    assert_eq!(settled.primary.calls, 2);
    assert_eq!(settled.completed_requests, 1);
    assert_eq!(settled.failed_primaries, 1);
    assert_eq!(settled.primary.unavailable_cost_calls, 1);
    assert_eq!(
        settled.primary.cost_usd, seed.primary.cost_usd,
        "never invent cancelled-request usage or money"
    );
    let evidence = serde_json::to_value(settled.request_evidence.as_ref().unwrap()).unwrap();
    assert_eq!(evidence["prior_untracked"], 0);
    let dashboard = serde_json::to_value(
        settled
            .request_evidence
            .as_ref()
            .unwrap()
            .snapshot()
            .dashboard(false),
    )
    .unwrap();
    assert_eq!(dashboard["whole_window"]["primary_responses"], 1);
    assert_eq!(dashboard["excluded_failed_calls"], 1);
    let mut restarted = checkpoint_service(directory.path());
    restarted.config = service.config.clone();
    let restarted = Arc::new(restarted);
    assert_eq!(load(&restarted, &id).unwrap().primary.calls, 2);
    let next = forward_request(
        restarted.clone(),
        headers,
        body,
        false,
        "cancel-followup".into(),
    )
    .await
    .unwrap();
    axum::body::to_bytes(next.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let final_state = load(&restarted, &id).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(final_state.primary.calls, 3);
    assert_eq!(final_state.completed_requests, 2);
    assert_eq!(final_state.failed_primaries, 1);
    assert_eq!(final_state.primary.input_tokens, 2000);
    assert_eq!(final_state.primary.cost_usd, 2.0 * seed.primary.cost_usd);
    let dashboard = serde_json::to_value(
        final_state
            .request_evidence
            .unwrap()
            .snapshot()
            .dashboard(false),
    )
    .unwrap();
    assert_eq!(dashboard["coverage"]["prior_untracked_calls"], 0);
    assert_eq!(dashboard["whole_window"]["primary_responses"], 2);
    provider.abort();
}
