// ADMISSION-001: execute dispatch, concurrent primary commit, merge and restart.
async fn background_commit_admission_control(kind: &str) {
    background_commit_admission_control_for_import(kind, false).await;
}

async fn background_commit_admission_control_for_import(kind: &str, bootstrap: bool) {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let memory = json!({"source_ids":["g2"],"text":"retained learning ".repeat(200)});
    let advice = match kind {
        "empty" => json!({"protected":[],"removable":[],"memories":[]}),
        "partial" => json!({"protected":[],"removable":["g2"],"memories":[memory]}),
        _ => json!({"protected":[],"removable":[],"memories":[memory]}),
    };
    let (signal, gate, requests) = (started.clone(), release.clone(), calls.clone());
    let response_advice = advice.clone();
    let router = Router::new()
        .route("/review", post(move || {
            let (signal, gate, requests, advice) =
                (signal.clone(), gate.clone(), requests.clone(), response_advice.clone());
            async move {
                let advice = if requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    signal.notify_one();
                    gate.notified().await;
                    advice
                } else {
                    json!({"protected":[],"removable":[],"memories":[]})
                };
                axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}],"usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":40}}}))
            }
        }))
        .route("/primary", post(|| async {
            axum::Json(json!({"id":"concurrent-primary","status":"completed","output":[],"usage":{"input_tokens":50,"output_tokens":1}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let id = pr154_session_id(kind);
    let mut service = checkpoint_service(directory.path());
    small_admission_config(&mut service.config);
    service.config.background_reviews = true;
    service.config.review_every_requests = 4;
    service.config.review_min_new_tokens = 10;
    service.config.classifier_url = Some(format!("http://{address}/review"));
    service.config.upstream_url = format!("http://{address}/primary");
    let mut session = admission_session(100, 4);
    if bootstrap {
        session.import_replayed_prefix();
        assert_eq!(session.bootstrap_end_id, 2);
    }
    // Explicitly initialize BEFORE observations: the source really is uncovered.
    admission::initialize(&mut session);
    let frontier = serde_json::to_value(&session.review_admission).unwrap();
    assert!(
        session
            .review_admission
            .as_ref()
            .unwrap()
            .source_ids
            .is_empty()
    );
    session.observe_groups(&session.groups());
    let before = session.clone();
    let mut input = session
        .history
        .iter()
        .map(|item| item.value.clone())
        .collect::<Vec<_>>();
    input.push(json!({"role":"user","content":"new task context ".repeat(200)}));
    let mut grown = session.clone();
    grown.ingest(&input).unwrap();
    let mut budget_probe = grown.clone();
    budget_probe
        .apply_advice(
            &grown.groups(),
            &json!({"protected":[],"removable":[],"memories":[memory]}),
        )
        .unwrap();
    let base_bytes = reviewer_serialized_identity(&memory_review_body(&service.config, &grown)).0;
    let memory_bytes =
        reviewer_serialized_identity(&memory_review_body(&service.config, &budget_probe)).0;
    service.config.classifier_max_input_bytes =
        (base_bytes + 2 * (memory_bytes - base_bytes) - 512) as u32;
    let mut full_current = grown.clone();
    full_current.observe_groups(&full_current.groups());
    assert!(
        review_fits(
            &service.config,
            &review_body(&service.config, &full_current)
        ),
        "native growth itself fits: no forced capacity wait may block the primary"
    );
    let mut snapshot_advice = before.clone();
    snapshot_advice
        .apply_advice_with_memory_admission(&before.groups(), &advice, |candidate| {
            memory_admission_fits(&service.config, candidate)
        })
        .unwrap();
    assert_eq!(
        snapshot_advice.review_feedback["rejected_count"], 0,
        "advice must fit the dispatched snapshot"
    );
    if kind != "empty" {
        assert_eq!(snapshot_advice.memories.len(), 1);
        let mut rejected = grown.clone();
        rejected.memories = snapshot_advice.memories.clone();
        rejected.refresh_memory_shadow();
        assert!(
            !memory_admission_fits(&service.config, &rejected),
            "only concurrent task growth exhausts memory headroom"
        );
    }
    assert!(admission::decision(&service.config, &session, &admission_main(&session), false)["new_native_source_estimated_tokens"].as_f64().unwrap() >= 10.0);
    let service = Arc::new(service);
    let main = admission_main(&session);
    dispatch_review(service.clone(), id.clone(), &mut session, main).await;
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    assert!(session.background_review_pending);
    // Use the actual primary forwarding/commit path while the review is gated.
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", kind.parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    let primary = tokio::time::timeout(
        Duration::from_secs(5),
        forward_request(
            service.clone(),
            headers,
            Bytes::from(serde_json::to_vec(&json!({"model":"gpt-6-luna","input":input})).unwrap()),
            false,
            "concurrent".into(),
        ),
    )
    .await
    .expect("compatible primary must finish while the review response is still gated")
    .unwrap();
    assert!(primary.status().is_success());
    axum::body::to_bytes(primary.into_body(), 1024 * 1024)
        .await
        .unwrap();
    // relay closes its body only after the real successful checkpoint commit.
    let current = load(&service, &id).unwrap();
    assert_eq!(current.completed_requests, 5);
    assert_eq!(current.primary.calls, 1);
    assert_eq!(
        current.last_response_id.as_deref(),
        Some("concurrent-primary")
    );
    assert_eq!(current.review_epoch, before.review_epoch);
    assert_eq!(current.history_rebases, before.history_rebases);
    for (old, new) in before.history.iter().zip(&current.history) {
        assert_eq!(
            (old.id, old.cohort, old.removed, &old.value),
            (new.id, new.cohort, new.removed, &new.value)
        );
    }
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.background_reviews.lock().await.contains(&id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let merged = load(&service, &id).unwrap();
    assert!(!merged.background_review_pending);
    assert_eq!(merged.shadow.calls, 1);
    assert_eq!(merged.shadow.unavailable_cost_calls, 0);
    assert_eq!(
        (
            merged.shadow.input_tokens,
            merged.shadow.output_tokens,
            merged.shadow.cached_tokens
        ),
        (100, 10, 40)
    );
    assert!(merged.shadow.cost_usd > 0.0);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    if bootstrap {
        assert!(merged.bootstrap_estimated_cost_usd > 0.0);
        assert_eq!(
            merged.bootstrap_reviewed_ids.contains(&2),
            kind != "rejected",
            "RI-001: current-budget wholly rejected bootstrap advice must not consume source"
        );
        assert_eq!(merged.bootstrap_review_complete, kind != "rejected");
        assert_eq!(merged.bootstrap_review_pending, kind == "rejected");
        assert_eq!(merged.bootstrap_end_id, before.bootstrap_end_id);
    }
    assert_eq!(
        merged.last_review_request, 4,
        "dispatch attempt consumes ordinary cadence, not merge-time age"
    );
    assert_eq!(merged.completed_requests, 5);
    assert_eq!(merged.primary.calls, 1);
    assert_eq!(merged.last_response_id, current.last_response_id);
    assert_eq!(merged.response_frontier, current.response_frontier);
    assert_eq!(merged.review_context, current.review_context);
    assert_eq!(
        serde_json::to_value(&merged.history).unwrap(),
        serde_json::to_value(&current.history).unwrap(),
        "merge retains the complete primary suffix and cohorts"
    );
    assert!(merged.memories.is_empty());
    assert_eq!(merged.review_feedback["accepted_memories"], 0);
    assert_eq!(
        merged.review_feedback["accepted_opinions"],
        if kind == "partial" { 1 } else { 0 }
    );
    assert_eq!(
        merged.review_feedback["rejected_count"],
        if kind == "empty" { 0 } else { 1 }
    );
    let entries = pr154_journal(directory.path(), &id);
    assert!(
        entries
            .iter()
            .any(|entry| entry["event"] == "background_review_merged"),
        "must merge compatibly, not discard stale advice"
    );
    let snapshot = entries
        .iter()
        .find(|entry| entry["event"] == "review_finished")
        .unwrap();
    assert_eq!(snapshot["data"]["valid"], true);
    if kind == "rejected" {
        assert_eq!(
            serde_json::to_value(&merged.review_admission).unwrap(),
            frontier,
            "ADMISSION-001: wholly rejected committed advice must not cover source or increase no-yield backoff"
        );
    } else {
        assert!(
            !merged
                .review_admission
                .as_ref()
                .unwrap()
                .source_ids
                .is_empty()
        );
        assert_eq!(
            merged.review_admission.as_ref().unwrap().no_yield_streak,
            if kind == "empty" { 1 } else { 0 }
        );
        if kind == "partial" {
            assert!(
                merged.drop_authorized(
                    &merged
                        .groups()
                        .into_iter()
                        .find(|group| group.id == 2)
                        .unwrap()
                )
            );
        }
    }
    let mut restarted = checkpoint_service(directory.path());
    small_admission_config(&mut restarted.config);
    restarted.config.review_every_requests = 4;
    restarted.config.review_min_new_tokens = 10;
    restarted.config.classifier_url = service.config.classifier_url.clone();
    restarted.config.classifier_max_input_bytes = service.config.classifier_max_input_bytes;
    drop(service);
    let mut resumed = load(&restarted, &id).unwrap();
    assert_eq!(
        serde_json::to_value(&resumed).unwrap(),
        serde_json::to_value(&merged).unwrap()
    );
    if kind == "rejected" && !bootstrap {
        let mut run = ReviewRun::default();
        let main = admission_main(&resumed);
        assert!(!review(&restarted, &id, &mut resumed, &main, false, &mut run).await);
        assert_eq!(
            resumed.shadow.calls, 1,
            "no immediate retry before ordinary cadence"
        );
        resumed.completed_requests = 8;
        let main = admission_main(&resumed);
        assert!(
            review(&restarted, &id, &mut resumed, &main, false, &mut run).await,
            "ordinary cadence retries the same uncovered source without new native growth"
        );
        assert_eq!(resumed.shadow.calls, 2);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(resumed.last_review_request, 8);
    }
    provider.abort();
}

#[tokio::test]
async fn adaptive_background_all_rejected_memory_preserves_uncovered_source_and_retry() {
    background_commit_admission_control("rejected").await;
}

#[tokio::test]
async fn adaptive_background_valid_empty_still_covers_source_and_backs_off() {
    background_commit_admission_control("empty").await;
}

#[tokio::test]
async fn adaptive_background_partial_valid_still_covers_source_and_resets_backoff() {
    background_commit_admission_control("partial").await;
}

#[tokio::test]
async fn bootstrap_background_all_rejected_memory_keeps_source_pending_after_restart() {
    background_commit_admission_control_for_import("rejected", true).await;
}

#[tokio::test]
async fn bootstrap_background_valid_empty_covers_source_after_restart() {
    background_commit_admission_control_for_import("empty", true).await;
}

#[tokio::test]
async fn bootstrap_background_partial_valid_covers_source_after_restart() {
    background_commit_admission_control_for_import("partial", true).await;
}
