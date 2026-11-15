// Actual transport/auth/review/primary owners, synthetic credentials only.
// The OAuth URL seam exists only in cfg(test); no new runtime configuration.
#[tokio::test]
async fn client_receipt_auth_retry_settles_each_actual_attempt_for_both_roles() {
    use base64::Engine;
    use std::sync::atomic::{AtomicUsize, Ordering};
    for reviewer in [false, true] {
        for refresh_ok in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let account = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}}"#,
            );
            let token = format!("e30.{account}.fixture");
            std::fs::write(
                directory.path().join("auth.json"),
                serde_json::to_vec(&json!({
                    "version":1,"access_token":token,"refresh_token":"synthetic-refresh",
                    "expires_at_ms":4102444800000u64
                }))
                .unwrap(),
            )
            .unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let refreshes = Arc::new(AtomicUsize::new(0));
            let (sent, mut wires) = mpsc::unbounded_channel();
            let router = Router::new()
                .route("/responses", post({
                    let calls = calls.clone();
                    move |body: Bytes| {
                        let calls = calls.clone();
                        let sent = sent.clone();
                        async move {
                            let n = calls.fetch_add(1, Ordering::SeqCst);
                            sent.send(body).unwrap();
                            let value = json!({"id":format!("retry-{n}"),
                                "status":if n == 0 {"failed"} else {"completed"},"output":[],
                                "usage":{"input_tokens":1000,"output_tokens":1,
                                "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0}}});
                            if n == 0 {
                                (StatusCode::UNAUTHORIZED, axum::Json(value)).into_response()
                            } else {
                                ([("content-type", "text/event-stream")],
                                    format!("event: response.completed\ndata: {}\n\n", json!({"type":"response.completed","response":value}))).into_response()
                            }
                        }
                    }
                }))
                .route("/token", post({
                    let token = token.clone();
                    let refreshes = refreshes.clone();
                    move || {
                        let token = token.clone();
                        let refreshes = refreshes.clone();
                        async move {
                            refreshes.fetch_add(1, Ordering::SeqCst);
                            if refresh_ok {
                                axum::Json(json!({"access_token":token,"refresh_token":"synthetic-next","expires_in":3600})).into_response()
                            } else {
                                StatusCode::UNAUTHORIZED.into_response()
                            }
                        }
                    }
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let mut service = checkpoint_service(directory.path());
            service.config.mode = Mode::Off;
            service.config.codex_login = true;
            service.config.codex_home = Some(directory.path().to_owned());
            service.config.classifier_url = None;
            service.config.upstream_url = format!("http://{address}/responses");
            service.codex_token_url = Some(format!("http://{address}/token"));
            service.config.review_min_primary_tokens = 1_000_000;
            service.config.classifier_timeout_secs = 3;
            service.config.request_timeout_secs = 3;
            let service = Arc::new(service);
            let mut session;
            let id;
            if reviewer {
                session = semantic_subject();
                session.request_evidence = Some(crate::proxy_savings::RequestEvidence::default());
                id = pr154_session_id("receipt-retry-review");
                let projection = planning::Projection::canonical(&session);
                let mut run = ReviewRun {
                    background: true,
                    request_id: "retry-review".into(),
                    ..Default::default()
                };
                tokio::time::timeout(
                    Duration::from_secs(5),
                    run_review(&service, &id, &mut session, &projection, &mut run),
                )
                .await
                .unwrap();
                assert_eq!(run.receipts.len(), if refresh_ok { 2 } else { 1 });
                let before = semantic_subject();
                let mut merged = before.clone();
                merged.request_evidence = Some(crate::proxy_savings::RequestEvidence::default());
                settle_review(&mut merged, &before, &session, &run);
                assert_eq!(merged.shadow.calls, session.shadow.calls);
                assert_eq!(merged.shadow.cost_usd, session.shadow.cost_usd);
                save(&service, &id, &merged).unwrap();
                session = merged;
            } else {
                let mut headers = HeaderMap::new();
                headers.insert("x-carry-session", "receipt-retry-primary".parse().unwrap());
                id = identity(&headers).unwrap().unwrap();
                let result = tokio::time::timeout(
                    Duration::from_secs(5),
                    forward_request(
                        service.clone(),
                        headers,
                        Bytes::from_static(br#"{"model":"gpt-6-luna","input":[],"stream":true}"#),
                        false,
                        "retry-primary".into(),
                    ),
                )
                .await
                .unwrap();
                if refresh_ok {
                    let response = result.unwrap();
                    axum::body::to_bytes(response.into_body(), 1024 * 1024)
                        .await
                        .unwrap();
                } else {
                    assert_eq!(result.unwrap_err().0, StatusCode::BAD_GATEWAY);
                }
                session = load(&service, &id).unwrap();
            }
            let count = if refresh_ok { 2 } else { 1 };
            assert_eq!(calls.load(Ordering::SeqCst), count);
            assert_eq!(refreshes.load(Ordering::SeqCst), 1);
            let first = wires.recv().await.unwrap();
            if refresh_ok {
                assert_eq!(
                    wires.recv().await.unwrap(),
                    first,
                    "retry reuses the exact final wire"
                );
            }
            assert!(wires.try_recv().is_err());
            let ledger = if reviewer {
                &session.shadow
            } else {
                &session.primary
            };
            assert_eq!(ledger.calls, count as u64);
            assert_eq!(ledger.input_tokens, 1000 * count as u64);
            assert_eq!(ledger.unavailable_cost_calls, 0);
            assert!((ledger.cost_usd - count as f64 * 0.0001005).abs() < 1e-12);
            let mut restored_service = checkpoint_service(directory.path());
            restored_service.config = service.config.clone();
            let restored = load(&restored_service, &id).unwrap();
            let restored_ledger = if reviewer {
                &restored.shadow
            } else {
                &restored.primary
            };
            assert_eq!(
                serde_json::to_value(restored_ledger).unwrap(),
                serde_json::to_value(ledger).unwrap()
            );
            let record = timing_record(
                directory.path(),
                &id,
                if reviewer {
                    "retry-review"
                } else {
                    "retry-primary"
                },
            );
            let attempts = record["data"]["attempts"].as_array().unwrap();
            assert_eq!(attempts.len(), count);
            assert_eq!(attempts[0]["http_status"], 401);
            assert_eq!(attempts[0]["receipt"]["provider_status"], "failed");
            assert_eq!(attempts[0]["receipt"]["pricing_available"], true);
            if refresh_ok {
                assert_eq!(attempts[1]["http_status"], 200);
                assert_eq!(attempts[1]["receipt"]["provider_status"], "completed");
                assert_ne!(
                    attempts[0]["receipt"]["attempt_id"],
                    attempts[1]["receipt"]["attempt_id"]
                );
                assert_eq!(
                    attempts[0]["receipt"]["contract_fingerprint"],
                    attempts[1]["receipt"]["contract_fingerprint"]
                );
            }
            assert!(!record.to_string().contains("synthetic-refresh"));
            server.abort();
        }
    }
}
