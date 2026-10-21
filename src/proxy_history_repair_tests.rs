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

#[tokio::test]
async fn history_paid_primary_abandoned_attempt_is_durably_censored_once() {
    use std::sync::atomic::AtomicUsize;
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let arrived = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (seen, held, go) = (calls.clone(), arrived.clone(), release.clone());
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let (seen, held, go) = (seen.clone(), held.clone(), go.clone());
            async move {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    held.notify_one();
                    go.notified().await;
                }
                axum::Json(json!({"id":format!("abandoned_{n}"),"status":"completed",
                "output":[],"usage":{"input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}}))
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
    service.config.history_max_bytes = 16384;
    let service = Arc::new(service);
    let input = json!([{"role":"user","content":"abandoned paid goal"}]);
    let pending_service = service.clone();
    let body =
        Bytes::from(serde_json::to_vec(&json!({"model":"gpt-6-luna","input":input})).unwrap());
    let pending = tokio::spawn(async move {
        forward_request(
            pending_service,
            HeaderMap::new(),
            body,
            false,
            logging::request_id(),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    let owner = history_race_owner(&service, "abandoned paid goal");
    let prior = std::fs::read(directory.path().join("history.index")).unwrap();
    pending.abort();
    let _ = pending.await;
    release.notify_one();
    let store: Value = serde_json::from_slice(&prior).unwrap();
    assert_eq!(
        store["snapshots"][&owner]["primary_pending"], true,
        "a paid transport must own a durable primary attempt before it can be abandoned"
    );
    // A new process/service has no worker or in-memory candidate. The next
    // actual call must preserve one unknown attempted call, never invent usage.
    let mut restarted = checkpoint_service(directory.path());
    restarted.config = service.config.clone();
    let restarted = Arc::new(restarted);
    history_race_send(restarted.clone(), input.clone()).await;
    let saved = load(&restarted, &owner).unwrap();
    assert_eq!(saved.primary.calls, 2);
    assert_eq!(saved.primary.unavailable_cost_calls, 2);
    assert_eq!(saved.primary.input_tokens, 100);
    assert_eq!(
        saved
            .cost_trajectory
            .as_ref()
            .unwrap()
            .snapshot()
            .coverage
            .prior_untracked_calls,
        1
    );
    history_race_send(restarted.clone(), input).await;
    let saved = load(&restarted, &owner).unwrap();
    assert_eq!(saved.primary.calls, 3);
    assert_eq!(saved.primary.unavailable_cost_calls, 3);
    assert_eq!(
        saved
            .cost_trajectory
            .as_ref()
            .unwrap()
            .snapshot()
            .coverage
            .prior_untracked_calls,
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(
        std::fs::read(directory.path().join("history.index"))
            .unwrap()
            .len()
            <= 16384
    );
    provider.abort();
}

#[tokio::test]
async fn history_paid_primary_oversized_settlement_survives_restart() {
    history_paid_growth(false, 32768).await;
}

#[tokio::test]
async fn history_paid_reviewer_oversized_settlement_survives_restart() {
    history_paid_growth(true, 32768).await;
}

#[tokio::test]
async fn history_paid_primary_fitting_growth_publishes_output() {
    history_paid_growth(false, 64).await;
}

#[tokio::test]
async fn history_paid_reviewer_fitting_growth_preserves_replay() {
    history_paid_growth(true, 64).await;
}

async fn history_paid_growth(reviewer: bool, growth: usize) {
    use std::sync::atomic::AtomicUsize;
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let reviews = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let reviewed = reviews.clone();
    let app = Router::new()
        .route("/v1/responses", post(move || {
            let seen = seen.clone();
            async move {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"id":format!("paid_{n}"),"status":"completed",
                    "output":if !reviewer && n == 0 {json!([{"role":"assistant","content":"X".repeat(growth)}])} else {json!([])},
                    "usage":{"input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}}))
            }
        }))
        .route("/review", post(move || {
            let reviewed = reviewed.clone();
            async move {
                reviewed.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"id":"paid_review","status":"completed",
                    "output":[{"type":"message","role":"assistant","content":[{"type":"output_text",
                    "text":format!("{}{{\"protected\":[],\"removable\":[],\"memories\":[]}}", " ".repeat(growth))}]}],
                    "usage":{"input_tokens":101,"output_tokens":2,"input_tokens_details":{"cached_tokens":0}}}))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut service = checkpoint_service(directory.path());
    service.config.upstream_url = format!("{endpoint}/v1/responses");
    service.config.classifier_url = Some(format!("{endpoint}/review"));
    service.config.classifier_model = "gpt-6-luna".into();
    service.config.review_every_requests = 100;
    service.config.bootstrap_min_input_bytes = usize::MAX;
    service.config.history_max_bytes = 16384;
    let service = Arc::new(service);
    let input = json!([{"role":"user","content":"paid growth goal"},
        {"role":"assistant","content":"already removed source"},
        {"role":"assistant","content":"still eligible source"},
        {"role":"user","content":"paid growth boundary"}]);
    let response = forward_request(
        service.clone(),
        HeaderMap::new(),
        Bytes::from(serde_json::to_vec(&json!({"model":"gpt-6-luna","input":input})).unwrap()),
        false,
        logging::request_id(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // A failed publication may truncate the client stream. Still inspect the
    // authoritative store, never the diagnostic mirror or a synthetic receipt.
    let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024).await;
    let owner = history_race_owner(&service, "paid growth goal");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "fitting predispatch state actually reached loopback"
    );
    if reviewer {
        let mut selected = load(&service, &owner).unwrap();
        selected.history[1].removed = true; // Explicit prior removal authority.
        save(&service, &owner, &selected).unwrap();
        let mut worker = (*service).clone();
        worker.config.review_every_requests = 1;
        dispatch_review_for(
            Arc::new(worker),
            owner.clone(),
            &mut selected,
            json!({"model":"gpt-6-luna","input":input}),
            logging::request_id(),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while service.background_reviews.lock().await.contains(&owner) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("real reviewer worker must finish settlement, not time out");
        assert_eq!(
            reviews.load(Ordering::SeqCst),
            1,
            "actual metered review required"
        );
    }
    let restarted = checkpoint_service(directory.path());
    let saved = load(&restarted, &owner).unwrap();
    assert_eq!(
        saved.primary.calls, 1,
        "paid primary must survive failed publication"
    );
    assert_eq!(saved.primary.input_tokens, 100);
    // The loopback route is deliberately not certified as an official priced
    // provider. Metered tokens are exact; dollar cost must remain unavailable.
    assert_eq!(saved.primary.unavailable_cost_calls, 1);
    assert_eq!(
        saved.shadow.calls,
        u64::from(reviewer),
        "paid reviewer must survive failed publication"
    );
    if reviewer {
        assert_eq!(saved.shadow.input_tokens, 101);
        assert_eq!(saved.shadow.unavailable_cost_calls, 1);
        assert!(!saved.background_review_pending);
        assert!(
            saved.history[1].removed,
            "settlement fallback cannot reset removal state"
        );
        if growth < 16384 {
            assert!(
                saved.review_history.is_some(),
                "fitting replay must not be discarded"
            );
        }
    } else if growth < 16384 {
        assert_eq!(saved.pending_output[0]["content"], "X".repeat(growth));
        assert_eq!(saved.last_response_id.as_deref(), Some("paid_0"));
    } else {
        assert!(
            saved.pending_output.is_empty(),
            "oversized uncommitted output is not replay authority"
        );
        let namespace = saved.history_namespace.as_deref().unwrap();
        let mut delta = json!({"input":[],"previous_response_id":"paid_0"});
        assert!(
            history::expand(&service, namespace, &mut delta).is_err(),
            "failed publication must not publish external IDs"
        );
    }
    let bytes = std::fs::read(directory.path().join("history.index")).unwrap();
    assert!(bytes.len() <= 16384);
    history_race_send(service.clone(), input).await;
    let saved = load(&restarted, &owner).unwrap();
    assert_eq!(
        saved.primary.calls, 2,
        "retry adds exactly one actual primary"
    );
    assert_eq!(
        saved.shadow.calls,
        u64::from(reviewer),
        "restart/retry cannot replay a paid review receipt"
    );
    if reviewer {
        assert!(saved.history[1].removed);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(reviews.load(Ordering::SeqCst), usize::from(reviewer));
    provider.abort();
}
