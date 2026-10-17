// Additional executable controls for hosted GREEN; not part of the six-test RED receipt.
fn pr154_journal(directory: &Path, id: &str) -> Vec<Value> {
    std::fs::read_to_string(directory.join("diagnostics").join(format!("{id}.jsonl")))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn pr154_journal_caps_are_independent_concurrent_and_restart_safe() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("untouched.jsonl");
    std::fs::write(&raw, b"old opted-in evidence\n").unwrap();
    let service = checkpoint_service(directory.path());
    save(&service, "checkpoint", &Session::default()).unwrap();
    let checkpoint = std::fs::read(directory.path().join("checkpoint.json")).unwrap();
    for _ in 0..2 {
        let service = checkpoint_service(directory.path());
        logging::maintain(&service);
        std::thread::scope(|scope| {
            for lane in 0..8 {
                let service = &service;
                scope.spawn(move || {
                    let data = json!({"counts":vec![123u64; 1500]});
                    for _ in 0..40 {
                        journal(
                            service,
                            &format!("lane-{lane}"),
                            "request",
                            "fixture",
                            &data,
                        );
                    }
                });
            }
        });
        let files = pr154_log_files(&directory.path().join("diagnostics"));
        assert!(!files.is_empty());
        let total: u64 = files
            .iter()
            .map(|path| std::fs::metadata(path).unwrap().len())
            .sum();
        assert!(total <= 256 * 1024);
        for path in files {
            assert!(std::fs::metadata(&path).unwrap().len() <= 64 * 1024);
            for line in std::fs::read_to_string(path).unwrap().lines() {
                assert!(line.len() + 1 <= 16 * 1024);
                serde_json::from_str::<Value>(line).unwrap();
            }
        }
    }
    assert_eq!(std::fs::read(&raw).unwrap(), b"old opted-in evidence\n");
    assert_eq!(
        std::fs::read(directory.path().join("checkpoint.json")).unwrap(),
        checkpoint
    );
}

#[test]
fn pr154_journal_oversize_io_and_partial_restart_are_fail_open() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    journal(&service, "lane", "request", "small", &json!({"count":1}));
    let path = directory.path().join("diagnostics/lane.jsonl");
    let before = std::fs::read(&path).unwrap();
    journal(
        &service,
        "lane",
        "request",
        "large",
        &json!({"counts":vec![123u64; 10000]}),
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "oversized records are dropped whole"
    );
    // Simulate a process dying mid-record; restart may evict that lane file.
    private_file(&path, true)
        .unwrap()
        .write_all(b"{\"partial\":")
        .unwrap();
    logging::maintain(&service);
    journal(&service, "lane", "request", "restarted", &json!({}));
    let events = pr154_journal(directory.path(), "lane");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "restarted");
    assert!(events[0]["dropped_before"].as_u64().unwrap() > 0);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    journal(&service, "lane", "request", "io_failure", &json!({}));
    save(&service, "lane", &Session::default()).unwrap();
    assert_eq!(load(&service, "lane").unwrap().completed_requests, 0);
    assert!(path.is_dir());
}

#[test]
fn pr154_verbose_legacy_maintenance_never_touches_checkpoint_or_symlink() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    save(&service, "lane", &Session::default()).unwrap();
    let checkpoint = std::fs::read(directory.path().join("lane.json")).unwrap();
    let raw = directory.path().join("legacy.jsonl");
    let oversized = json!({"input":"x".repeat(100 * 1024)}).to_string() + "\n";
    std::fs::write(&raw, oversized.as_bytes()).unwrap();
    let temporary = directory.path().join(".lane.tmp");
    std::fs::write(&temporary, b"checkpoint temporary").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        directory.path().join("lane.json"),
        directory.path().join("link.jsonl"),
    )
    .unwrap();
    logging::maintain(&service);
    assert_eq!(std::fs::read(&raw).unwrap(), oversized.as_bytes());
    service.config.verbose = true;
    logging::maintain(&service);
    assert!(
        !raw.exists(),
        "legacy oversized record must enter the opt-in cap"
    );
    assert_eq!(
        std::fs::read(directory.path().join("lane.json")).unwrap(),
        checkpoint
    );
    assert_eq!(std::fs::read(temporary).unwrap(), b"checkpoint temporary");
    #[cfg(unix)]
    assert!(
        std::fs::symlink_metadata(directory.path().join("link.jsonl"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn pr154_raw_retention_and_verbose_restart_preserve_independent_jsonl() {
    let directory = tempfile::tempdir().unwrap();
    let legacy_id = format!("{:x}", Sha256::digest(b"pr154-legacy-trace"));
    let active_id = format!("{:x}", Sha256::digest(b"pr154-active-trace"));
    let legacy = directory.path().join(format!("{legacy_id}.jsonl"));
    let active = directory.path().join(format!("{active_id}.jsonl"));
    // Valid, small JSON records exceed the raw lane budget only in aggregate.
    let record = json!({"padding":"x".repeat(1024)}).to_string() + "\n";
    let bytes = record.repeat(300).into_bytes();
    assert!(bytes.len() > 256 * 1024);
    let artifacts =
        ["benchmark-events.jsonl", "unrelated.jsonl"].map(|name| directory.path().join(name));
    let mut observations = Vec::new();
    for restart in [false, true] {
        // Reseed both paths so restart is exercised even when append deletes them.
        for path in &artifacts {
            std::fs::write(path, &bytes).unwrap();
        }
        std::fs::write(&legacy, &bytes).unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.verbose = true;
        if restart {
            logging::maintain(&service);
        } else {
            trace(&service, &active_id, "fixture", &json!({})).unwrap();
            assert!(active.is_file(), "the real raw writer must run");
        }
        assert!(!legacy.exists(), "owned hash-named traces must still evict");
        observations.push((
            restart,
            artifacts.each_ref().map(|path| std::fs::read(path).ok()),
        ));
    }
    for (restart, observed) in observations {
        for (path, actual) in artifacts.iter().zip(observed) {
            assert!(
                actual.as_deref() == Some(bytes.as_slice()),
                "independent artifact {} must be byte-preserved (restart={restart})",
                path.display()
            );
        }
    }
}

#[test]
fn pr154_usage_diagnostics_are_numeric_and_preserve_censoring() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    for response in [
        json!({"status":"PRIVATE_STATUS","service_tier":"PRIVATE_TIER","usage":{"input_tokens":10,"output_tokens":1,"input_tokens_details":{"cached_tokens":0,"private":"PRIVATE_USAGE"},"private":"PRIVATE_USAGE"},"output":[{"name":"PRIVATE_TOOL"}]}),
        json!({"status":"completed","usage":{"input_tokens":10,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}}),
        json!({"status":"completed","usage":{"input_tokens":10,"output_tokens":1}}),
        json!({"status":"completed","usage":{"input_tokens":"PRIVATE_USAGE"}}),
    ] {
        logging::usage(
            &service,
            "lane",
            "request",
            "primary",
            Some(&response),
            "gpt-6-luna",
            true,
        );
    }
    let events = pr154_journal(directory.path(), "lane");
    assert_eq!(events.len(), 4);
    let data: Vec<_> = events.iter().map(|event| &event["data"]).collect();
    assert_eq!(data[0]["usage_available"], true);
    assert_eq!(data[0]["cost_censored"], true);
    assert_eq!(data[1]["cost_available"], true);
    assert_eq!(data[2]["usage_available"], true);
    assert_eq!(data[2]["cost_censored"], true);
    assert_eq!(data[3]["usage_available"], false);
    assert_eq!(data[3]["cost_censored"], true);
    assert!(!serde_json::to_string(&events).unwrap().contains("PRIVATE_"));
}

#[tokio::test]
async fn pr154_skip_reasons_execute_real_review_gates_without_provider_calls() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.classifier_url = Some("http://127.0.0.1:1/review".into());
    let mut session = Session::default();
    let mut run = ReviewRun {
        request_id: "request".into(),
        ..ReviewRun::default()
    };
    assert!(!review(&service, "lane", &mut session, &json!({}), false, &mut run).await);
    session.completed_requests = 1;
    assert!(!review(&service, "lane", &mut session, &json!({}), false, &mut run).await);
    observe_source(&mut session, "goal");
    observe_source(&mut session, "PRIVATE_EVIDENCE");
    observe_source(&mut session, "current");
    service.config.classifier_max_input_bytes = 1;
    assert!(!review(&service, "lane", &mut session, &json!({}), false, &mut run).await);
    let reasons: Vec<_> = pr154_journal(directory.path(), "lane")
        .iter()
        .map(|event| event["data"]["reason"].clone())
        .collect();
    assert_eq!(
        reasons,
        vec![
            json!("cadence"),
            json!("no_eligible_sources"),
            json!("reviewer_capacity")
        ]
    );
    assert_eq!(session.shadow.calls, 0);
}

#[tokio::test]
async fn pr154_background_timeline_keeps_trigger_id_and_stale_spend() {
    for stale in [false, true] {
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());
        let (gate, signal) = (release.clone(), started.clone());
        let router = Router::new().route("/review", post(move || {
            let (gate, signal) = (gate.clone(), signal.clone());
            async move {
                signal.notify_one();
                gate.notified().await;
                axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":json!({"protected":[],"removable":["g2"],"memories":[]}).to_string()}]}],"usage":{"input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.classifier_url = Some(format!("http://{address}/review"));
        let service = Arc::new(service);
        let mut session = Session::default();
        observe_source(&mut session, "goal");
        observe_source(&mut session, "PRIVATE_EVIDENCE");
        observe_source(&mut session, "current");
        session.completed_requests = 1;
        dispatch_review_for(
            service.clone(),
            "lane".into(),
            &mut session,
            json!({}),
            "trigger".into(),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        dispatch_review_for(
            service.clone(),
            "lane".into(),
            &mut session,
            json!({}),
            "later".into(),
        )
        .await;
        if stale {
            let _lock = session_lock(&service, "lane").await;
            let mut current = load(&service, "lane").unwrap();
            current.reset_active();
            save(&service, "lane", &current).unwrap();
        }
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.background_reviews.lock().await.contains("lane") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let saved = load(&service, "lane").unwrap();
        assert_eq!(saved.shadow.calls, 1);
        assert_eq!(saved.shadow.unavailable_cost_calls, 0);
        assert!(!saved.background_review_pending);
        assert_eq!(saved.opinions.is_empty(), stale);
        let entries = pr154_journal(directory.path(), "lane");
        let expected = if stale {
            "background_review_discarded"
        } else {
            "background_review_merged"
        };
        for event in [
            "background_review_dispatched",
            "review_started",
            "review_finished",
            expected,
        ] {
            let entry = entries
                .iter()
                .find(|entry| entry["event"] == event)
                .unwrap();
            assert_eq!(entry["request_id"], "trigger");
        }
        assert!(
            entries
                .iter()
                .any(|entry| entry["event"] == "review_skipped"
                    && entry["data"]["reason"] == "background_running"
                    && entry["request_id"] == "later")
        );
        assert!(
            !serde_json::to_string(&entries)
                .unwrap()
                .contains("PRIVATE_")
        );
        provider.abort();
    }
}

#[tokio::test]
async fn pr154_selected_main_audit_rollback_and_reviewer_only_use_real_forwarding() {
    for scenario in ["audit", "main", "failed", "reviewer_only", "journal_io"] {
        let failure = scenario == "failed";
        let router = Router::new()
            .route("/responses", post(move || async move {
                (if failure { StatusCode::TOO_MANY_REQUESTS } else { StatusCode::OK }, axum::Json(json!({
                    "status":if failure {"failed"} else {"completed"}, "output":[],
                    "usage":{"input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}
                })))
            }))
            .route("/review", post(|| async {
                axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":json!({"protected":[],"removable":[],"memories":[]}).to_string()}]}],"usage":{"input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}}))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.upstream_url = format!("http://{address}/responses");
        service.config.classifier_url = Some(format!("http://{address}/review"));
        service.config.mode = if scenario == "audit" {
            Mode::Audit
        } else {
            Mode::Compact
        };
        service.config.min_payback_percent = 0;
        service.config.payoff_requests = 1;
        let mut session = Session::default();
        observe_source(&mut session, "goal");
        let evidence = if scenario == "reviewer_only" {
            "evidence".to_owned()
        } else {
            "PRIVATE_EVIDENCE ".repeat(2000)
        };
        observe_source(&mut session, &evidence);
        observe_source(&mut session, "current");
        session.completed_requests = 1;
        if scenario == "reviewer_only" {
            let mut sources = review_source_input(&session, &session.groups());
            sources.pop();
            let mut input = sources.clone();
            input.push(json!({"role":"assistant","content":"PRIVATE_REVIEWER ".repeat(4000)}));
            session.review_history = Some(crate::proxy_state::ReviewHistory {
                source_len: sources.len(),
                source_sha256: reviewer_serialized_identity(&sources).1,
                input,
            });
            service.config.classifier_max_input_tokens = 5000;
        } else {
            session
                .apply_advice(
                    &session.groups(),
                    &json!({"protected":[],"removable":["g2"],"memories":[]}),
                )
                .unwrap();
        }
        let main = json!({"model":"gpt-6-luna","input":session.render_primary()});
        let mut headers = HeaderMap::new();
        headers.insert("x-carry-session", "fixture".parse().unwrap());
        let id = identity(&headers).unwrap().unwrap();
        save(&service, &id, &session).unwrap();
        if scenario == "journal_io" {
            std::fs::write(directory.path().join("diagnostics"), b"not a directory").unwrap();
        }
        let service = Arc::new(service);
        let response = forward_request(
            service.clone(),
            headers,
            Bytes::from(serde_json::to_vec(&main).unwrap()),
            false,
            "trigger".into(),
        )
        .await
        .unwrap();
        assert_eq!(response.status().is_success(), !failure);
        axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let saved = load(&service, &id).unwrap();
        let committed_main = scenario == "main" || scenario == "journal_io";
        assert_eq!(saved.compactions, u64::from(committed_main), "{scenario}");
        assert_eq!(
            saved.completed_requests,
            if failure { 1 } else { 2 },
            "{scenario}"
        );
        if scenario == "journal_io" {
            provider.abort();
            continue;
        }
        let events = pr154_journal(directory.path(), &id);
        let selected = events
            .iter()
            .find(|event| event["event"] == "compaction_selected")
            .expect("fixture must select a real plan");
        assert_eq!(
            selected["data"]["main_changed"],
            scenario != "reviewer_only"
        );
        assert_eq!(selected["data"]["apply_enabled"], scenario != "audit");
        if failure {
            assert!(
                !events
                    .iter()
                    .any(|event| event["event"] == "primary_committed")
            );
            assert!(events.iter().any(|event| event["event"] == "primary_failed"
                && event["data"]["candidate_discarded"] == true));
        } else {
            let committed = events
                .iter()
                .find(|event| event["event"] == "primary_committed")
                .unwrap();
            assert_eq!(committed["data"]["main_changed"], committed_main);
        }
        if scenario == "reviewer_only" {
            assert_eq!(saved.reviewer_compactions, 1);
            assert!(
                events
                    .iter()
                    .any(|event| event["event"] == "reviewer_state_committed"
                        && event["data"]["reviewer_compactions"] == 1
                        && event["data"]["main_compaction_pending"] == false)
            );
        }
        assert!(!serde_json::to_string(&events).unwrap().contains("PRIVATE_"));
        assert!(events.iter().all(|event| event["request_id"] == "trigger"));
        provider.abort();
    }
}
