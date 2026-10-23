// RI-001: real async import chunks retain their own commit-boundary validity.
async fn bootstrap_multichunk_commit_control(memory_first: bool) {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (signal, gate, requests) = (started.clone(), release.clone(), calls.clone());
    let router = Router::new()
        .route("/review", post(move || {
            let (signal, gate, requests) = (signal.clone(), gate.clone(), requests.clone());
            async move {
                let call = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert!(call < 2, "bounded two-chunk import");
                if call == 1 {
                    signal.notify_one();
                    gate.notified().await;
                }
                let memory = (call == 0) == memory_first;
                let advice = json!({"protected":[],"removable":[],"memories":if memory {
                    vec![json!({"source_ids":[if call == 0 {"g2"} else {"g4"}],"text":"retained learning ".repeat(200)})]
                } else {vec![]}});
                axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}],"usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":40}}}))
            }
        }))
        .route("/primary", post(|| async {
            axum::Json(json!({"id":"multichunk-primary","status":"completed","output":[],"usage":{"input_tokens":50,"output_tokens":1}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let id = pr154_session_id(if memory_first {
        "memory-first"
    } else {
        "memory-last"
    });
    let mut service = checkpoint_service(directory.path());
    small_admission_config(&mut service.config);
    service.config.review_max_calls = 2;
    service.config.classifier_url = Some(format!("http://{address}/review"));
    service.config.upstream_url = format!("http://{address}/primary");
    let mut session = Session::default();
    let history = vec![
        json!({"role":"user","content":"durable goal"}),
        json!({"role":"assistant","content":"first evidence ".repeat(100)}),
        json!({"role":"user","content":"old direction"}),
        json!({"role":"assistant","content":"second evidence ".repeat(400)}),
        json!({"role":"user","content":"current direction"}),
    ];
    session.ingest(&history).unwrap();
    session.import_replayed_prefix();
    admission::initialize(&mut session);
    // A large prior diagnostic consumes the first chunk's suffix capacity. The
    // first completed review replaces it, admitting the second chunk without
    // deleting or rewriting any canonical source.
    session.review_feedback =
        json!({"rejected_count":1,"rejected":[{"reason":"previous diagnostic ".repeat(1000)}]});
    let groups = session.groups();
    service.config.classifier_max_input_bytes = (reviewer_serialized_identity(
        &review_body_for_groups(&service.config, &session, &groups[..3]),
    )
    .0 + 256) as u32;
    assert!(review_fits(
        &service.config,
        &review_body_for_groups(&service.config, &session, &groups[..3])
    ));
    assert!(!review_fits(
        &service.config,
        &review_body_for_groups(&service.config, &session, &groups[..4])
    ));
    let before = session.clone();
    let service = Arc::new(service);
    let config = &service.config;
    let main = admission_main(&session);
    dispatch_review(service.clone(), id.clone(), &mut session, main).await;
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let in_flight = load(&service, &id).unwrap();
    assert!(in_flight.background_review_pending);
    assert!(in_flight.bootstrap_reviewed_ids.is_empty());
    assert_eq!(
        in_flight.shadow.calls, 0,
        "worker-local first chunk is not yet durable"
    );
    let restart_probe = checkpoint_service(directory.path());
    assert_eq!(
        serde_json::to_value(load(&restart_probe, &id).unwrap()).unwrap(),
        serde_json::to_value(&in_flight).unwrap()
    );
    let mut grown = history;
    grown.push(json!({"role":"user","content":"new task context ".repeat(1000)}));
    let mut current_probe = before.clone();
    current_probe.ingest(&grown).unwrap();
    let memory_advice = json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"retained learning ".repeat(200)}]});
    let mut snapshot_probe = before.clone();
    snapshot_probe
        .apply_advice_with_memory_admission(&before.groups(), &memory_advice, |s| {
            memory_admission_fits(config, s)
        })
        .unwrap();
    assert_eq!(
        snapshot_probe.memories.len(),
        1,
        "memory fits the dispatched snapshot"
    );
    current_probe.memories = snapshot_probe.memories.clone();
    current_probe.refresh_memory_shadow();
    assert!(
        !memory_admission_fits(config, &current_probe),
        "current task growth rejects the memory"
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-carry-session",
        if memory_first {
            "memory-first"
        } else {
            "memory-last"
        }
        .parse()
        .unwrap(),
    );
    headers.insert("content-type", "application/json".parse().unwrap());
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        forward_request(
            service.clone(),
            headers,
            Bytes::from(serde_json::to_vec(&json!({"model":"gpt-6-luna","input":grown})).unwrap()),
            false,
            "multichunk".into(),
        ),
    )
    .await
    .expect("primary must complete while the second chunk is held")
    .unwrap();
    assert!(response.status().is_success());
    axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let primary = load(&service, &id).unwrap();
    assert_eq!(primary.primary.calls, 1);
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.background_reviews.lock().await.contains(&id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let merged = load(&service, &id).unwrap();
    assert_eq!(merged.shadow.calls, 2);
    assert_eq!(merged.shadow.unavailable_cost_calls, 0);
    assert_eq!(
        (
            merged.shadow.input_tokens,
            merged.shadow.output_tokens,
            merged.shadow.cached_tokens
        ),
        (200, 20, 80)
    );
    assert!(merged.shadow.cost_usd > 0.0);
    assert!(merged.bootstrap_estimated_cost_usd > 0.0);
    assert!(merged.memories.is_empty());
    assert_eq!(
        serde_json::to_value(&merged.history).unwrap(),
        serde_json::to_value(&primary.history).unwrap()
    );
    assert_eq!(merged.primary.calls, 1);
    assert_eq!(merged.last_response_id, primary.last_response_id);
    assert_eq!(merged.response_frontier, primary.response_frontier);
    assert!(!merged.background_review_pending);
    let expected = if memory_first { vec![4] } else { vec![1, 2, 3] };
    assert_eq!(
        merged
            .bootstrap_reviewed_ids
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        expected,
        "RI-001: bootstrap coverage must use each chunk's current validity, not final feedback"
    );
    assert!(merged.bootstrap_review_pending);
    assert!(!merged.bootstrap_review_complete);
    assert!(
        !merged
            .review_admission
            .as_ref()
            .unwrap()
            .source_ids
            .is_empty(),
        "the valid-empty chunk still covers its ordinary source"
    );
    // The rejected replacement now preserves its source: protection progress,
    // not removal yield. The valid-empty chunk's coverage remains independent.
    assert_eq!(merged.review_admission.as_ref().unwrap().no_yield_streak, 0);
    let source = if memory_first { 2 } else { 4 };
    assert_eq!(
        merged.opinions.get(&source).map(String::as_str),
        Some("keep")
    );
    let restarted = checkpoint_service(directory.path());
    drop(service);
    assert_eq!(
        serde_json::to_value(load(&restarted, &id).unwrap()).unwrap(),
        serde_json::to_value(&merged).unwrap()
    );
    let finished = pr154_journal(directory.path(), &id)
        .into_iter()
        .filter(|e| e["event"] == "review_finished")
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 2);
    assert!(finished.iter().all(|e| e["data"]["valid"] == true));
    provider.abort();
}

// HTTP-FORK-003: publication is stricter than ordinary partial advice. Execute
// the real first-import worker over loopback, not a synthetic last-feedback merge.
async fn fork_multichunk_publication_control(partial_first: bool, empty_last: bool) {
    let captures = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = captures.clone();
    let router = Router::new().route("/review", post(move |axum::Json(body): axum::Json<Value>| {
        let requests = requests.clone();
        async move {
            let mut captures = requests.lock().await;
            captures.push(body);
            let call = captures.len();
            assert!(call <= 2, "exactly two real import chunks");
            let advice = if call == 1 {
                json!({"protected":[],"removable":["g2"],"memories":[{
                    "source_ids":[if partial_first {"g999"} else {"g2"}],
                    "text":"PORTABLE_FACT"}]})
            } else {
                json!({"protected":[],"removable":if empty_last {vec![]} else {vec!["g4"]},"memories":[]})
            };
            axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}],"usage":{"input_tokens":100,"output_tokens":10}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let id = pr154_session_id("fork-multichunk");
    let mut service = checkpoint_service(directory.path());
    small_admission_config(&mut service.config);
    service.config.mode = Mode::Compact;
    service.config.review_max_calls = 2;
    service.config.classifier_url = Some(format!("http://{address}/review"));
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"durable goal"}),
            json!({"role":"assistant","content":"first evidence ".repeat(100)}),
            json!({"role":"user","content":"old direction"}),
            json!({"role":"assistant","content":"second evidence ".repeat(400)}),
            json!({"role":"user","content":"current direction"}),
        ])
        .unwrap();
    session.import_replayed_prefix();
    let main = admission_main(&session);
    session.review_context = cache_base(&main);
    admission::initialize(&mut session);
    session.review_feedback =
        json!({"rejected_count":1,"rejected":[{"reason":"previous diagnostic ".repeat(1000)}]});
    let groups = session.groups();
    service.config.classifier_max_input_bytes = (reviewer_serialized_identity(
        &review_body_for_groups(&service.config, &session, &groups[..3]),
    )
    .0 + 256) as u32;
    assert!(review_fits(
        &service.config,
        &review_body_for_groups(&service.config, &session, &groups[..3])
    ));
    assert!(!review_fits(
        &service.config,
        &review_body_for_groups(&service.config, &session, &groups[..4])
    ));
    let before = session.clone();
    let service = Arc::new(service);
    dispatch_review(service.clone(), id.clone(), &mut session, main).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.background_reviews.lock().await.contains(&id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let merged = load(&service, &id).unwrap();
    let captures = captures.lock().await;
    assert_eq!(captures.len(), 2);
    assert!(!captures[0].to_string().contains("second evidence"));
    assert!(captures[1].to_string().contains("second evidence"));
    assert_eq!(merged.shadow.calls, 2);
    assert_eq!(
        merged.review_feedback["rejected_count"], 0,
        "last chunk really is fully accepted"
    );
    assert_eq!(merged.partial_reviews, u64::from(partial_first));
    assert!(merged.bootstrap_review_complete);
    assert_eq!(
        serde_json::to_value(&merged.history).unwrap(),
        serde_json::to_value(&before.history).unwrap()
    );
    let groups = merged.groups();
    let dropped = groups.iter().find(|g| g.id == 2).unwrap();
    assert!(
        merged.drop_authorized(dropped),
        "ordinary partially accepted opinion remains usable"
    );
    let mut ordinary = merged.clone();
    ordinary.remove(&[2], &groups);
    assert!(
        !serde_json::to_string(&ordinary.render_primary())
            .unwrap()
            .contains("first evidence")
    );
    assert!(before.opinions.is_empty() && before.memories.is_empty());
    let finished = pr154_journal(directory.path(), &id)
        .into_iter()
        .filter(|e| e["event"] == "review_finished")
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 2);
    assert_eq!(finished[0]["data"]["accepted_opinions"], 1);
    assert_eq!(
        finished[0]["data"]["rejected_count"],
        u64::from(partial_first)
    );
    assert_eq!(finished[1]["data"]["rejected_count"], 0);
    provider.abort();
    assert_eq!(
        merged.fork_points.is_empty(),
        partial_first,
        "HTTP-FORK-003: any rejected chunk forbids portable publication, even after clean final feedback"
    );
    if !partial_first {
        assert_eq!(
            merged.fork_points.len(),
            1,
            "fully accepted multi-chunk control publishes"
        );
        assert_eq!(merged.memories.len(), 1);
    }
}

#[tokio::test]
async fn fork_multichunk_partial_then_empty_never_publishes() {
    fork_multichunk_publication_control(true, true).await;
}

#[tokio::test]
async fn fork_multichunk_partial_then_accepted_never_publishes() {
    fork_multichunk_publication_control(true, false).await;
}

#[tokio::test]
async fn fork_multichunk_fully_accepted_publishes() {
    fork_multichunk_publication_control(false, false).await;
}

#[tokio::test]
async fn bootstrap_multichunk_rejected_memory_then_valid_empty_preserves_chunk_coverage() {
    bootstrap_multichunk_commit_control(true).await;
}

#[tokio::test]
async fn bootstrap_multichunk_valid_empty_then_rejected_memory_preserves_chunk_coverage() {
    bootstrap_multichunk_commit_control(false).await;
}
