fn timing_record(directory: &Path, id: &str, request: &str) -> Value {
    let records: Vec<_> = pr154_journal(directory, id)
        .into_iter()
        .filter(|e| e["event"] == "phase_timing" && e["request_id"] == request)
        .collect();
    assert_eq!(records.len(), 1, "terminal summary must be unique");
    records[0].clone()
}

#[tokio::test]
async fn phase_timing_rejection_and_cancelled_lock_wait_preserve_unknowns() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.mode = Mode::Off;
    let (tx, mut waits) = mpsc::unbounded_channel();
    *service.lock_waits.lock().unwrap() = Some(tx);
    let service = Arc::new(service);
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "PRIVATE_SESSION".parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    let rejected = forward_request(
        service.clone(),
        headers.clone(),
        Bytes::from_static(b"not JSON"),
        false,
        "reject".into(),
    )
    .await;
    assert_eq!(rejected.unwrap_err().0, StatusCode::BAD_REQUEST);
    let record = timing_record(directory.path(), &id, "reject");
    let t = &record["data"];
    assert_eq!(t["outcome"], "rejected");
    assert_eq!(t["incomplete"], false);
    assert_eq!(t["attempts"], json!([]));
    assert_eq!(t["first_body_us"], Value::Null);
    assert_eq!(t["preparation_done_us"], Value::Null);
    let lock = session_lock(&service, &id).await;
    waits.recv().await.unwrap();
    let task = tokio::spawn({
        let service = service.clone();
        async move {
            forward_request(
                service,
                headers,
                Bytes::from_static(br#"{"model":"gpt-6-luna","input":[]}"#),
                false,
                "abort".into(),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), waits.recv())
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(lock);
    let record = timing_record(directory.path(), &id, "abort");
    let t = &record["data"];
    assert_eq!(t["outcome"], "cancelled");
    assert_eq!(t["incomplete"], true);
    assert_eq!(t["active_phase"], "primary_lock_wait");
    assert_eq!(
        t["primary_lock_wait_us"],
        Value::Null,
        "unfinished wait is not zero or a completed span"
    );
    assert_eq!(t["attempts"], json!([]));
    assert_eq!(load(&service, &id).unwrap().primary.calls, 0);
    assert!(!record.to_string().contains("PRIVATE_"));
}

#[tokio::test]
async fn phase_timing_http_error_and_client_disconnect_are_not_successes() {
    for disconnect in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        let router = Router::new().route(
            "/responses",
            post(move || {
                let first_tx = first_tx.clone();
                async move {
                    if !disconnect {
                        return (StatusCode::TOO_MANY_REQUESTS, "PRIVATE_ERROR_BODY")
                            .into_response();
                    }
                    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(1);
                    tokio::spawn(async move {
                        tx.send(Ok(Bytes::from_static(b"data: PRIVATE_BODY\n\n")))
                            .await
                            .unwrap();
                        first_tx.send(()).unwrap();
                        tx.closed().await;
                    });
                    (
                        [("content-type", "text/event-stream")],
                        Body::from_stream(ReceiverStream::new(rx)),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.mode = Mode::Off;
        service.config.upstream_url =
            format!("http://{}/responses", listener.local_addr().unwrap());
        let service = Arc::new(service);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut headers = HeaderMap::new();
        headers.insert("x-carry-session", "PRIVATE_SESSION".parse().unwrap());
        let id = identity(&headers).unwrap().unwrap();
        let response = forward_request(
            service.clone(),
            headers,
            Bytes::from_static(br#"{"model":"gpt-6-luna","input":[]}"#),
            false,
            "terminal".into(),
        )
        .await
        .unwrap();
        if disconnect {
            first_rx.recv().await.unwrap();
            drop(response);
            // The production relay continues censored accounting after disconnect.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if load(&service, &id).unwrap().failed_primaries == 1 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        } else {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap(),
                "PRIVATE_ERROR_BODY"
            );
        }
        let record = timing_record(directory.path(), &id, "terminal");
        let t = &record["data"];
        assert_eq!(
            t["outcome"],
            if disconnect {
                "client_disconnected"
            } else {
                "http_error"
            }
        );
        assert_eq!(t["incomplete"], disconnect);
        assert_eq!(t["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(t["body_complete_us"].is_null(), disconnect);
        assert_eq!(load(&service, &id).unwrap().failed_primaries, 1);
        assert!(!record.to_string().contains("PRIVATE_"));
        server.abort();
    }
}

#[tokio::test]
async fn phase_timing_reviewer_is_correlated_background_and_timeout_is_censored() {
    let (url, mut captures, provider) = review_capture_provider().await;
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.classifier_url = Some(url);
    let id = pr154_session_id("review-timing");
    let mut session = semantic_subject();
    let projection = planning::Projection::canonical(&session);
    let expected = review_body_projected(&service.config, &session, &projection);
    let mut run = ReviewRun {
        background: true,
        request_id: "parent".into(),
        ..Default::default()
    };
    assert!(run_review(&service, &id, &mut session, &projection, &mut run).await);
    assert_eq!(captures.recv().await.unwrap(), expected);
    assert_eq!(run.receipts.len(), 1);
    let record = timing_record(directory.path(), &id, "parent");
    let t = &record["data"];
    assert_eq!(t["role"], "reviewer");
    assert_eq!(t["background"], true);
    assert_eq!(t["blocking_review_us"], Value::Null);
    assert_eq!(t["outcome"], "complete");
    assert_eq!(t["incomplete"], false);
    assert_eq!(
        record["review_attempt_id"],
        session.last_reviewer_dispatch.as_ref().unwrap().request_id
    );
    assert!(t["native_attempt_id"].as_str().is_some());
    assert!(t["body_complete_us"].as_u64().unwrap() <= t["total_us"].as_u64().unwrap());
    provider.abort();

    // Real subscription owner deadline during held auth lock: no auth file,
    // refresh or external request can be reached.
    service.config.codex_login = true;
    service.config.classifier_url = None;
    service.config.codex_home = Some(directory.path().to_path_buf());
    service.config.classifier_timeout_secs = 1;
    let _auth = session_lock(&service, "codex-auth").await;
    let mut timed_out = ReviewRun {
        request_id: "deadline".into(),
        ..Default::default()
    };
    assert!(!run_review(&service, &id, &mut session, &projection, &mut timed_out).await);
    let record = timing_record(directory.path(), &id, "deadline");
    let t = &record["data"];
    assert_eq!(t["incomplete"], true);
    assert_eq!(t["attempts"], json!([]));
    assert_eq!(t["first_body_us"], Value::Null);
    assert_eq!(t["body_complete_us"], Value::Null);
    assert_eq!(session.shadow.calls, 2);
    assert_eq!(session.shadow.unavailable_cost_calls, 1);
}

#[tokio::test]
async fn phase_timing_retry_span_owner_keeps_attempts_distinct_without_double_counting() {
    // The timing owner exercised with real HTTP. This is not an OAuth refresh
    // or a new retry policy: production send_codex already owns that policy.
    let router = Router::new()
        .route("/first", post(|| async { StatusCode::UNAUTHORIZED }))
        .route("/second", post(|| async { "SYNTHETIC_RESPONSE" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let id = pr154_session_id("retry-timing");
    {
        let mut t = timing::Phases::new(service.clone(), "retry", None);
        t.session_id = id.clone();
        for path in ["first", "second"] {
            t.dispatch();
            let mut response = service
                .client
                .post(format!("http://{address}/{path}"))
                .send()
                .await
                .unwrap();
            t.headers(response.status());
            if path == "second" {
                while let Some(bytes) = response.chunk().await.unwrap() {
                    t.first_body(&bytes);
                }
                t.body_complete();
            }
        }
        t.outcome = "complete";
    }
    let record = timing_record(directory.path(), &id, "retry");
    let t = &record["data"];
    let attempts = t["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["ordinal"], 0);
    assert_eq!(attempts[0]["status"], 401);
    assert_eq!(attempts[0]["body_complete_us"], Value::Null);
    assert_eq!(attempts[1]["ordinal"], 1);
    assert_eq!(attempts[1]["status"], 200);
    assert!(
        attempts[1]["dispatch_us"].as_u64().unwrap() >= attempts[0]["headers_us"].as_u64().unwrap()
    );
    assert_eq!(t["body_complete_us"], attempts[1]["body_complete_us"]);
    assert_eq!(t["incomplete"], false);
    assert_eq!(
        load(&service, &id).unwrap().primary.calls,
        0,
        "timing never meters paid attempts"
    );
    server.abort();
}
