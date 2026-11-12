// Retention promises cannot be overridden by the continuation working-set cap.
#[test]
fn a_retention_existing_owner_admission_completes_without_lock_recursion() {
    let mut failures = Vec::new();
    for mode in ["prefix-candidate", "external-candidate", "resolve"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "proxy::tests::retention_existing_owner_admission_child",
                "--nocapture",
            ])
            .env("CARRY_TEST_RETENTION_ADMISSION_MODE", mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut timed_out = false;
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                timed_out = true;
                child.kill().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Bypass libtest capture: other tests may be stalled on the same
        // production lock, so preserve setup errors and qualified outcomes.
        use std::io::Write as _;
        if !stderr.contains("retention-admission: durable owner and direct pin ready") {
            writeln!(
                std::io::stderr(),
                "retention-admission: {mode} fixture preconditions failed: {output:?}"
            )
            .unwrap();
            panic!("{mode} failed fixture preconditions: {output:?}");
        }
        writeln!(
            std::io::stderr(),
            "retention-admission: {mode} seed_ready=true timed_out={timed_out} status={:?}",
            output.status
        )
        .unwrap();
        if timed_out || !output.status.success() {
            failures.push(format!("{mode}: timed_out={timed_out}, {output:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "existing-owner admission must complete with the original owner: {failures:?}"
    );
}

#[test]
fn retention_existing_owner_admission_child() {
    let Ok(mode) = std::env::var("CARRY_TEST_RETENTION_ADMISSION_MODE") else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let input = vec![json!({"role":"user","content":"retained continuation"})];
    let body = json!({"input":input});
    let (id, pin) = history::resolve(&service, "admission-namespace", &body).unwrap();
    let mut session = Session {
        history_shared: true,
        history_namespace: Some("admission-namespace".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    session.ingest(&input).unwrap();
    numeric_primary(&mut session, 0);
    let response = json!({"id":"retained-response","output":[]});
    history::committed(&service, &id, &session, Some((&input, &response))).unwrap();
    drop(pin);
    drop(history::pin(&service, &id).unwrap());
    let frozen = history::restore(&service, &id).unwrap();
    assert_eq!(frozen.render_primary(), input);
    assert_eq!(frozen.primary.calls, 1);
    assert!((frozen.primary.cost_usd - 0.002).abs() < 1e-12);
    eprintln!("retention-admission: durable owner and direct pin ready");
    let (selected, pin) = match mode.as_str() {
        "prefix-candidate" => history::candidate(&service, "admission-namespace", &body)
            .unwrap()
            .expect("existing prefix candidate"),
        "external-candidate" => history::candidate(
            &service,
            "admission-namespace",
            &json!({"input":[],"previous_response_id":"retained-response"}),
        )
        .unwrap()
        .expect("existing external-ID candidate"),
        "resolve" => history::resolve(&service, "admission-namespace", &body).unwrap(),
        _ => panic!("unknown fixture mode"),
    };
    assert_eq!(selected, id);
    drop(pin);
    assert_eq!(
        serde_json::to_value(history::restore(&service, &id).unwrap()).unwrap(),
        serde_json::to_value(frozen).unwrap()
    );
}

#[test]
fn retention_unreadable_newest_advice_never_resurrects_older_permission() {
    unreadable_advice_control(false);
}
#[test]
fn retention_oversized_newest_advice_never_resurrects_older_permission() {
    unreadable_advice_control(true);
}
fn unreadable_advice_control(oversized: bool) {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    let mut subject = semantic_subject();
    subject.review_epoch = 10;
    history::publish(&service, &subject, &subject).unwrap();
    let mut input = subject.render_primary();
    input.push(json!({"role":"user","content":"newer cut"}));
    subject.ingest(&input).unwrap();
    subject.review_epoch = 11;
    subject.review_feedback = json!({"padding":"x".repeat(1024)});
    history::publish(&service, &subject, &subject).unwrap();
    assert_eq!(
        history::advice(&service, "fixture-owner", &input)
            .unwrap()
            .unwrap()
            .generation,
        11
    );
    let db = rusqlite::Connection::open(directory.path().join("history.index")).unwrap();
    let newest: String = db.query_row("SELECT key FROM records WHERE kind='reviews' ORDER BY json_extract(CAST(data AS TEXT),'$.generation') DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    if oversized {
        let length: u64 = db
            .query_row(
                "SELECT length(data) FROM records WHERE kind='reviews' AND key=?1",
                [&newest],
                |r| r.get(0),
            )
            .unwrap();
        service.config.history_max_bytes = u32::try_from(length - 1).unwrap();
    } else {
        db.execute(
            "UPDATE records SET data=?1 WHERE kind='reviews' AND key=?2",
            rusqlite::params![b"corrupt".as_slice(), newest],
        )
        .unwrap();
    }
    let advice = history::advice(&service, "fixture-owner", &input);
    assert!(
        !advice.as_ref().is_ok_and(|r| r.is_some()),
        "unreadable newest authority must not resurrect generation 10: {advice:?}"
    );
}

#[test]
fn retention_oversized_existing_owner_is_backpressure_not_a_cold_miss() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    let input = vec![json!({"role":"user","content":"retained owner"})];
    let mut session = Session {
        history_shared: true,
        history_namespace: Some("oversized-owner".into()),
        ..Session::default()
    };
    session.ingest(&input).unwrap();
    history::committed(&service, "original-owner", &session, None).unwrap();
    let body = json!({"input":input});
    let (owner, pin) = history::resolve(&service, "oversized-owner", &body).unwrap();
    assert_eq!(owner, "original-owner");
    drop(pin);
    let db = rusqlite::Connection::open(directory.path().join("history.index")).unwrap();
    let length: u64 = db
        .query_row(
            "SELECT length(data) FROM records WHERE kind='snapshots'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    service.config.history_max_bytes = u32::try_from(length - 1).unwrap();
    match history::resolve(&service, "oversized-owner", &body) {
        Err(error) => assert_eq!(error.0, StatusCode::INSUFFICIENT_STORAGE),
        Ok((owner, _)) => panic!("oversized retained owner silently resolved as {owner}"),
    }
}

#[test]
fn retention_existing_unversioned_sqlite_is_rejected_without_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.index");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES('preserve');",
    )
    .unwrap();
    drop(db);
    let original = std::fs::read(&path).unwrap();
    let service = checkpoint_service(directory.path());
    assert!(
        history::ReadView::new(&service).is_err(),
        "a nominal authority read initialized an unsupported database"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn retention_invalid_predecessor_record_types_preserve_original_authority() {
    for kind in ["nodes", "tips", "reviews", "transcripts", "upstream_cache"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.index");
        let mut index = json!({"format":"carry-prefix-history-v4", "nodes":{}, "tips":{}, "reviews":{}, "transcripts":{}, "snapshots":{}, "upstream_cache":[]});
        if kind == "upstream_cache" {
            index[kind] = json!({"invalid":"cache"});
        } else {
            index[kind]["invalid-record"] = json!({"unexpected":true});
        }
        let original = serde_json::to_vec(&index).unwrap();
        std::fs::write(&path, &original).unwrap();
        let service = checkpoint_service(directory.path());
        assert!(
            history::initialize(&service).is_err(),
            "invalid {kind} replaced predecessor authority"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}

#[test]
fn retention_predecessor_defaulted_maps_import_in_isolated_processes() {
    let mut failures = Vec::new();
    for mode in ["explicit", "no-transcripts", "no-snapshots", "neither"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "proxy::tests::retention_predecessor_defaulted_maps_child",
                "--nocapture",
            ])
            .env("CARRY_TEST_PREDECESSOR_DEFAULTS", mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut timed_out = false;
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                timed_out = true;
                child.kill().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("predecessor-defaults: fixture ready"),
            "{mode} fixture setup failed: {output:?}"
        );
        if timed_out || !output.status.success() {
            failures.push(format!("{mode}: timed_out={timed_out}, {output:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "valid predecessor optional-map defaults must import without panic: {failures:?}"
    );
}

#[test]
fn retention_predecessor_defaulted_maps_child() {
    let Ok(mode) = std::env::var("CARRY_TEST_PREDECESSOR_DEFAULTS") else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.index");
    let mut index = json!({"format":"carry-prefix-history-v4", "nodes":{}, "tips":{}, "reviews":{}, "transcripts":{}, "snapshots":{}, "upstream_cache":[]});
    if mode == "no-transcripts" || mode == "neither" {
        index.as_object_mut().unwrap().remove("transcripts");
    }
    if mode == "no-snapshots" || mode == "neither" {
        index.as_object_mut().unwrap().remove("snapshots");
    }
    std::fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();
    let service = checkpoint_service(directory.path());
    eprintln!("predecessor-defaults: fixture ready");
    history::initialize(&service).unwrap();
    history::initialize(&service).unwrap();
    let mut session = Session {
        history_shared: true,
        history_namespace: Some("defaulted-predecessor".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    session
        .ingest(&[json!({"role":"user","content":"after migration"})])
        .unwrap();
    numeric_primary(&mut session, 0);
    history::committed(&service, "defaulted-owner", &session, None).unwrap();
    let restored = history::restore(&service, "defaulted-owner").unwrap();
    assert_eq!(restored.render_primary(), session.render_primary());
    assert_eq!(
        serde_json::to_value(&restored.primary).unwrap(),
        serde_json::to_value(&session.primary).unwrap()
    );
}

#[test]
fn retention_dangling_predecessor_frontier_preserves_original_authority() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.index");
    let index = json!({"format":"carry-prefix-history-v4", "nodes":{}, "tips":{ "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": {"session":"explicit-owner", "namespace":"observed-v1:fixture", "at":5_000_000}}, "reviews":{}, "transcripts":{}, "snapshots":{}, "upstream_cache":[]});
    let original = serde_json::to_vec(&index).unwrap();
    std::fs::write(&path, &original).unwrap();
    let service = checkpoint_service(directory.path());
    assert!(
        history::initialize(&service).is_err(),
        "dangling typed frontier replaced predecessor authority"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

fn historical_retention_fixture(service: &Service, owner: &str, with_seed: bool) -> Session {
    let mut session = Session {
        history_shared: true,
        history_namespace: Some("historical-retention".into()),
        completed_requests: 1,
        ..Session::default()
    };
    session
        .ingest(&[json!({"role":"user","content":"historical source"})])
        .unwrap();
    session.primary.calls = 1;
    session.primary.cost_usd = 0.002;
    let mut evidence = crate::proxy_savings::RequestEvidence::default();
    evidence.prior_untracked(1);
    session.request_evidence = Some(evidence);
    if with_seed {
        let estimate = crate::proxy_savings::Estimate {
            observed_primary_cost: Some(0.002),
            coverage: crate::proxy_savings::Coverage {
                primary_responses: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let seed = json!({"format":"carry-historical-success-seed-v1", "prefixes":[{
            "source_sha256":"a".repeat(64), "owner":owner, "history_namespace":"historical-retention",
            "primary_attempts":1, "reviewer_attempts":0, "estimate":estimate
        }]});
        std::fs::write(
            service.config.state_dir.join("historical-success.seed"),
            serde_json::to_vec(&seed).unwrap(),
        )
        .unwrap();
    }
    session
}

#[test]
fn retention_import_accounts_validated_historical_success_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let owner = "b".repeat(64);
    let session = historical_retention_fixture(&service, &owner, true);
    let session = recover_success_prefix(&service, &owner, session).unwrap();
    let expected = serde_json::to_value(
        session
            .request_evidence
            .as_ref()
            .unwrap()
            .snapshot()
            .dashboard(false),
    )
    .unwrap();
    assert_eq!(expected["observed_primary_cost"], 0.002);
    save(&service, &owner, &session).unwrap();
    let path = directory.path().join("history.index");
    let predecessor = history::fixture::read_index(&path);
    std::fs::write(&path, predecessor).unwrap();
    std::fs::remove_file(directory.path().join("historical-success.seed")).unwrap();
    history::initialize(&service).unwrap();
    let page = history::accounting(&service, 0, "", 100).unwrap();
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        page["records"][0]["estimate"], expected,
        "import must validate embedded historical proof before freezing the accounting observation"
    );
}

#[test]
fn retention_unchanged_coverage_dispatch_does_not_duplicate_accounting() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let owner = "b".repeat(64);
    let mut session = historical_retention_fixture(&service, &owner, false);
    save(&service, &owner, &session).unwrap();
    let first = history::accounting(&service, 0, "", 100).unwrap()["records"][0].clone();
    session
        .request_evidence
        .as_mut()
        .unwrap()
        .dispatched_selection(
            "next-unsettled-request",
            &HashSet::new(),
            None,
            false,
            Some(0),
        );
    assert_eq!(
        serde_json::to_value(
            session
                .request_evidence
                .as_ref()
                .unwrap()
                .snapshot()
                .dashboard(false)
        )
        .unwrap(),
        first["estimate"],
        "dispatch alone did not change settled numeric coverage"
    );
    save(&service, &owner, &session).unwrap();
    let records = history::accounting(&service, 0, "", 100).unwrap()["records"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        records.len(),
        1,
        "dispatch provenance is not another settled accounting observation"
    );
    assert_eq!(
        records[0], first,
        "the original observation remains immutable"
    );
}

#[test]
fn retention_evidence_transition_adds_observation_without_rewriting_money() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let owner = "b".repeat(64);
    let session = historical_retention_fixture(&service, &owner, false);
    save(&service, &owner, &session).unwrap();
    let first = history::accounting(&service, 0, "", 100).unwrap()["records"][0].clone();
    historical_retention_fixture(&service, &owner, true);
    let validated = load(&service, &owner).unwrap();
    assert_eq!(
        serde_json::to_value(
            validated
                .request_evidence
                .as_ref()
                .unwrap()
                .snapshot()
                .dashboard(false)
        )
        .unwrap()["observed_primary_cost"],
        0.002
    );
    save(&service, &owner, &validated).unwrap();
    let records = history::accounting(&service, 0, "", 100).unwrap()["records"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        records.len(),
        2,
        "new validated evidence must not be suppressed by unchanged paid counters"
    );
    assert_eq!(
        records[0], first,
        "old frozen money/provenance must remain immutable"
    );
    assert_eq!(records[1]["primary"], records[0]["primary"]);
    assert_eq!(records[1]["estimate"]["observed_primary_cost"], 0.002);
    save(&service, &owner, &validated).unwrap();
    assert_eq!(
        history::accounting(&service, 0, "", 100).unwrap()["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn retention_predecessor_import_preserves_money_bindings_and_rejects_capacity_atomically() {
    at_decision_clock(&[5_000_000], || {
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        let input =
            vec![json!({"role":"user","content":"predecessor observed source ".repeat(8192)})];
        let body = json!({"model":"gpt-6.1-sol","input":input});
        let namespace = history::namespace(&service, &HeaderMap::new(), &body).unwrap();
        let mut session = Session {
            history_shared: true,
            history_namespace: Some(namespace.clone()),
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        session.ingest(&input).unwrap();
        numeric_primary(&mut session, 0);
        save(&service, "predecessor-owner", &session).unwrap();
        let response = json!({"id":"predecessor-response","output":[{"role":"assistant","content":"original output"}]});
        let loaded = history::restore(&service, "predecessor-owner").unwrap();
        history::committed(
            &service,
            "predecessor-owner",
            &loaded,
            Some((&input, &response)),
        )
        .unwrap();
        let frozen =
            serde_json::to_value(history::restore(&service, "predecessor-owner").unwrap()).unwrap();
        let path = directory.path().join("history.index");
        // Frozen v4 wire shape: shared compact snapshots, exact-prefix nodes,
        // external-ID bindings and original reference timestamps, no new tables.
        let predecessor = history::fixture::read_index(&path);
        std::fs::write(&path, &predecessor).unwrap();
        service.config.history_disk_bytes = 65536;
        let rejected = history::namespace(&service, &HeaderMap::new(), &body).unwrap_err();
        assert_eq!(
            rejected.0,
            StatusCode::INSUFFICIENT_STORAGE,
            "supported predecessor must reach atomic disk admission, not be discarded as unknown state"
        );
        assert_eq!(std::fs::read(&path).unwrap(), predecessor);
        service.config.history_disk_bytes = 8 * 1024 * 1024;
        for _ in 0..2 {
            assert_eq!(
                history::namespace(&service, &HeaderMap::new(), &body).unwrap(),
                namespace
            );
            assert_eq!(
                serde_json::to_value(history::restore(&service, "predecessor-owner").unwrap())
                    .unwrap(),
                frozen
            );
            let mut resume = json!({"input":[],"previous_response_id":"predecessor-response"});
            history::expand(&service, &namespace, &mut resume).unwrap();
            let mut expected = input.clone();
            expected.extend(response["output"].as_array().unwrap().clone());
            assert_eq!(resume["input"], json!(expected));
        }
    });
}

#[tokio::test]
async fn retention_concurrent_real_primaries_keep_paid_fallback_under_full_store_pressure() {
    let (sent, mut captures) = mpsc::unbounded_channel();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_calls = calls.clone();
    let router = Router::new().route("/responses", post(move |body: Bytes| {
        let sent = sent.clone();
        let calls = provider_calls.clone();
        async move {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // A genuine complete receipt, but an output node above the unchanged
            // per-record bound. Normal semantic publication must fail; only the
            // bounded numeric settlement may use the admitted owner's promise.
            let response = json!({"id":format!("paid-fallback-{n}"),"status":"completed",
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"o".repeat(70000)}]}],
                "usage":{"input_tokens":10000,"output_tokens":7,"input_tokens_details":{"cached_tokens":9000}}});
            let (release, wait) = tokio::sync::oneshot::channel();
            sent.send((body, release, response.clone())).unwrap();
            wait.await.unwrap();
            axum::Json(response)
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    // Off mode intentionally uses a non-shared checkpoint and cannot qualify
    // shared-owner settlement. Audit preserves the canonical wire and owner;
    // the explicit floor suppresses review without leaving that lifecycle.
    service.config.mode = Mode::Audit;
    service.config.review_min_primary_tokens = 1_000_000;
    service.config.upstream_url = url;
    service.config.history_max_bytes = 65536;
    service.config.history_max_nodes = 512;
    service.config.history_disk_bytes = 24 * 1024 * 1024;
    let service = Arc::new(service);
    let pressure_pin = history::pin(&service, "primary-pressure").unwrap();
    let mut owners = Vec::new();
    let mut releases = Vec::new();
    let mut requests = Vec::new();
    for label in ["primary-a", "primary-b"] {
        let input = (0..128)
            .map(|n| {
                json!({
                    "role":if n % 2 == 0 {"user"} else {"assistant"},
                    "content":format!("{label} retained source {n}")
                })
            })
            .collect::<Vec<_>>();
        let body = json!({"model":"gpt-6-luna","input":input,"stream":false});
        let namespace = history::namespace(&service, &HeaderMap::new(), &body).unwrap();
        let (id, pin) = history::resolve(&service, &namespace, &body).unwrap();
        let mut seed = Session {
            history_shared: true,
            history_namespace: Some(namespace.clone()),
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        seed.ingest(&input).unwrap();
        save(&service, &id, &seed).unwrap();
        drop(pin);
        let wire = Bytes::from(serde_json::to_vec(&body).unwrap());
        let request_service = service.clone();
        let request_wire = wire.clone();
        requests.push(tokio::spawn(async move {
            let response = forward_request(
                request_service,
                HeaderMap::new(),
                request_wire,
                false,
                label.into(),
            )
            .await?;
            let status = response.status();
            let delivered = axum::body::to_bytes(response.into_body(), 1024 * 1024).await;
            Ok::<_, Failure>((
                status,
                delivered
                    .map(|bytes| bytes.len())
                    .map_err(|error| error.to_string()),
            ))
        }));
        let (captured, release, response) =
            tokio::time::timeout(Duration::from_secs(10), captures.recv())
                .await
                .expect("real primary must pass admission before capacity pressure")
                .unwrap();
        assert_eq!(
            captured, wire,
            "whole real primary wire, not a staged receipt"
        );
        let pending = history::restore(&service, &id).unwrap();
        assert_eq!(pending.render_primary(), input);
        assert!(
            pending.history_shared,
            "real request must retain the seeded shared owner"
        );
        assert_eq!(
            pending.history_namespace.as_deref(),
            Some(namespace.as_str())
        );
        assert!(pending.primary_pending);
        assert_eq!(pending.primary.calls, 0);
        assert_eq!(pending.shadow.calls, 0);
        assert_eq!(pending.primary.cost_usd, 0.0);
        owners.push((id, namespace, input, response, label));
        releases.push(release);
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(captures.try_recv().is_err());
    let mut accepted = 0;
    let mut refused = false;
    for n in 0..1024 {
        let mut pressure = Session {
            history_shared: true,
            history_namespace: Some("primary-pressure".into()),
            ..Session::default()
        };
        pressure
            .ingest(&[json!({"role":"user","content":format!("{n} {}", "x".repeat(48000))})])
            .unwrap();
        match history::committed(&service, "primary-pressure", &pressure, None) {
            Ok(()) => accepted += 1,
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
                refused = true;
                break;
            }
        }
    }
    assert!(
        accepted > 0 && refused,
        "real unchanged-cap publication pressure"
    );
    for (id, _, input, _, _) in &owners {
        let held = history::restore(&service, id).unwrap();
        assert_eq!(held.render_primary(), *input);
        assert!(held.primary_pending);
        assert_eq!(held.primary.calls, 0);
    }
    for release in releases {
        release.send(()).unwrap();
    }
    let mut delivery = Vec::new();
    for request in requests {
        delivery.push(
            tokio::time::timeout(Duration::from_secs(20), request)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(captures.try_recv().is_err());
    let mut restarted = checkpoint_service(directory.path());
    restarted.config = service.config.clone();
    let mut outcomes = Vec::new();
    for (id, namespace, input, response, label) in &owners {
        let durable = history::restore(&restarted, id).unwrap();
        assert_eq!(
            durable.render_primary(),
            *input,
            "failed publication cannot replace old ancestry"
        );
        assert_eq!(
            durable.shadow.calls, 0,
            "audit admission floor must not dispatch reviewers"
        );
        let mut resume = json!({"input":[],"previous_response_id":response["id"]});
        assert!(
            history::expand(&restarted, namespace, &mut resume).is_err(),
            "failed publication must not leak response bindings"
        );
        let events = timing_journal(directory.path(), id, label);
        let failures = events
            .iter()
            .filter(|event| event["event"] == "primary_checkpoint_failed")
            .collect::<Vec<_>>();
        assert_eq!(
            failures.len(),
            1,
            "qualify actual production fallback, not only direct settlement"
        );
        assert_eq!(failures[0]["data"]["candidate_committed"], false);
        outcomes.push(
            json!({"id":id,"primary":durable.primary,"pending":durable.primary_pending,
            "completed":durable.completed_requests,"failed":durable.failed_primaries,
            "fallback":failures[0]["data"]["accounting_committed"]}),
        );
    }
    let accounting = history::accounting(&restarted, 0, "", 100).unwrap();
    let bytes: u64 = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    provider.abort();
    drop(pressure_pin);
    assert!(bytes <= service.config.history_disk_bytes);
    // Finish both owners' wire, ancestry and fallback controls before grading
    // exact known money; an early money failure must not hide the sibling.
    eprintln!(
        "real-primary-fallback: delivery={delivery:?}, outcomes={outcomes:?}, accounting={accounting}"
    );
    for outcome in &outcomes {
        assert_eq!(outcome["fallback"], true);
        assert_eq!(outcome["pending"], false);
        assert_eq!(outcome["primary"]["calls"], 1);
        assert_eq!(outcome["primary"]["input_tokens"], 10000);
        assert_eq!(outcome["primary"]["output_tokens"], 7);
        assert_eq!(outcome["primary"]["cached_tokens"], 9000);
        assert_eq!(outcome["primary"]["unavailable_cost_calls"], 0);
        assert!((outcome["primary"]["cost_usd"].as_f64().unwrap() - 0.0001935).abs() < 1e-12);
        assert_eq!(outcome["completed"], 1);
        assert_eq!(outcome["failed"], 0);
        let records = accounting["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|record| record["owner"] == outcome["id"])
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            1,
            "one immutable paid observation per real owner"
        );
        assert_eq!(records[0]["primary"], outcome["primary"]);
    }
}

#[tokio::test]
async fn retention_concurrent_paid_reviews_keep_reserved_headroom_under_long_path_pressure() {
    let (url, mut captures, provider) = review_capture_provider().await;
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.classifier_url = Some(url);
    service.config.history_max_bytes = 65536;
    service.config.history_max_nodes = 512;
    service.config.history_disk_bytes = 24 * 1024 * 1024;
    let service = Arc::new(service);
    // This third writer is admitted before either paid request. It may consume
    // its own capacity, never the headroom already promised to paid owners.
    let pressure_pin = history::pin(&service, "pressure-owner").unwrap();
    let (waiter, mut waits) = tokio::sync::mpsc::unbounded_channel();
    *service.lock_waits.lock().unwrap() = Some(waiter);
    let ids = ["paid-long-path-a", "paid-long-path-b"];
    let mut locks = Vec::new();
    let mut originals = Vec::new();
    for id in ids {
        let input = (0..128)
            .map(|n| {
                json!({"role":if n % 2 == 0 {"user"} else {"assistant"},
                "content":format!("{id} source {n}")})
            })
            .collect::<Vec<_>>();
        let mut session = Session {
            history_shared: true,
            history_namespace: Some(id.into()),
            review_context: json!({"model":"gpt-6-luna"}),
            bootstrap_review_pending: true,
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        session.ingest(&input).unwrap();
        for item in &mut session.history {
            item.exposed = true;
        }
        numeric_primary(&mut session, 0);
        locks.push(session_lock(&service, id).await);
        let projection = planning::Projection::canonical(&session);
        let expected = review_body_projected(&service.config, &session, &projection);
        dispatch_review_for(
            service.clone(),
            id.into(),
            &mut session,
            projection,
            format!("{id}-review"),
        )
        .await;
        let captured = tokio::time::timeout(Duration::from_secs(10), captures.recv())
            .await
            .expect("admitted long-path reviewer must reach loopback provider")
            .unwrap();
        assert_eq!(
            captured, expected,
            "pressure fixture must preserve the whole paid wire"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), waits.recv())
                .await
                .unwrap()
                .unwrap(),
            id,
            "hold a completed real paid receipt at the existing settlement lock"
        );
        let durable = history::restore(&service, id).unwrap();
        assert!(durable.background_review_pending);
        assert_eq!(durable.shadow.calls, 0);
        assert_eq!(durable.primary.calls, 1);
        assert!((durable.primary.cost_usd - 0.002).abs() < 1e-12);
        assert_eq!(durable.render_primary(), input);
        originals.push(durable);
    }
    assert!(
        captures.try_recv().is_err(),
        "exactly two upstream calls before pressure"
    );
    let mut rejected = false;
    let mut accepted = 0;
    for n in 0..1024 {
        let mut pressure = Session {
            history_shared: true,
            history_namespace: Some("pressure-writer".into()),
            ..Session::default()
        };
        pressure
            .ingest(&[json!({"role":"user", "content":format!("{n} {}", "x".repeat(48000))})])
            .unwrap();
        match history::committed(&service, "pressure-owner", &pressure, None) {
            Ok(()) => accepted += 1,
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
                rejected = true;
                break;
            }
        }
    }
    assert!(
        accepted > 0 && rejected,
        "reach genuine bounded disk pressure without changing caps"
    );
    let bytes: u64 = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(
        bytes <= service.config.history_disk_bytes,
        "physical store exceeded disk bound: {bytes}"
    );
    for (id, original) in ids.iter().zip(&originals) {
        let held = history::restore(&service, id).unwrap();
        assert_eq!(held.render_primary(), original.render_primary());
        assert_eq!(
            serde_json::to_value(&held.primary).unwrap(),
            serde_json::to_value(&original.primary).unwrap()
        );
    }
    drop(locks);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !service.background_reviews.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("paid settlements must terminate under bounded capacity");
    assert!(
        captures.try_recv().is_err(),
        "pressure/settlement cannot invent more model calls"
    );
    drop(pressure_pin);
    let mut restarted = checkpoint_service(directory.path());
    restarted.config.history_max_bytes = service.config.history_max_bytes;
    restarted.config.history_max_nodes = service.config.history_max_nodes;
    restarted.config.history_disk_bytes = service.config.history_disk_bytes;
    let mut lost = Vec::new();
    for (id, original) in ids.iter().zip(&originals) {
        let durable = history::restore(&restarted, id).unwrap();
        assert_eq!(
            durable.render_primary(),
            original.render_primary(),
            "pressure cannot evict paid ancestry"
        );
        assert_eq!(
            serde_json::to_value(&durable.primary).unwrap(),
            serde_json::to_value(&original.primary).unwrap()
        );
        if durable.shadow.calls != 1
            || durable.shadow.input_tokens != 10000
            || durable.shadow.output_tokens != 7
            || durable.shadow.cached_tokens != 9000
            || durable.shadow.unavailable_cost_calls != 0
            || (durable.shadow.cost_usd - 0.0001935).abs() > 1e-12
            || durable.background_review_pending
        {
            lost.push(json!({"owner":id,"reviewer":durable.shadow,"pending":durable.background_review_pending}));
        }
    }
    provider.abort();
    assert!(
        lost.is_empty(),
        "admitted concurrent paid receipts lost their reserved settlement headroom: {lost:?}"
    );
}

#[test]
fn retention_unchanged_long_path_remains_writable_under_other_owner_pressure() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.history_max_bytes = 65536;
    service.config.history_max_nodes = 512;
    service.config.history_disk_bytes = 8 * 1024 * 1024;
    let mut retained = Session {
        history_shared: true,
        history_namespace: Some("long-path-retained".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    let input = (0..128)
        .map(|n| json!({"role":"user","content":format!("{n} {}", "x".repeat(96))}))
        .collect::<Vec<_>>();
    retained.ingest(&input).unwrap();
    numeric_primary(&mut retained, 0);
    let retained_pin = history::pin(&service, "long-path-retained").unwrap();
    history::committed(&service, "long-path-retained", &retained, None).unwrap();
    let pressure_pin = history::pin(&service, "long-path-pressure").unwrap();
    let mut accepted = 0;
    let mut refused = false;
    for n in 0..256 {
        let mut pressure = Session {
            history_shared: true,
            history_namespace: Some("long-path-pressure".into()),
            ..Session::default()
        };
        pressure
            .ingest(&[json!({"role":"user","content":format!("{n} {}", "p".repeat(48000))})])
            .unwrap();
        match history::committed(&service, "long-path-pressure", &pressure, None) {
            Ok(()) => accepted += 1,
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
                refused = true;
                break;
            }
        }
    }
    assert!(accepted > 0 && refused, "qualify real publication pressure");
    let frozen = history::restore(&service, "long-path-retained").unwrap();
    assert_eq!(frozen.render_primary(), input);
    assert_eq!(frozen.primary.calls, 1);
    assert!((frozen.primary.cost_usd - 0.002).abs() < 1e-12);
    let accounting = history::accounting(&service, 0, "", 100).unwrap();
    let publication = history::committed(&service, "long-path-retained", &retained, None);
    // Complete the frozen money, source and independent-accounting controls
    // before grading whether unchanged ancestry was charged again.
    assert_eq!(
        serde_json::to_value(history::restore(&service, "long-path-retained").unwrap()).unwrap(),
        serde_json::to_value(frozen).unwrap()
    );
    assert_eq!(
        history::accounting(&service, 0, "", 100).unwrap(),
        accounting
    );
    let bytes: u64 = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(bytes <= service.config.history_disk_bytes);
    assert!(
        publication.is_ok(),
        "unchanged retained ancestry must not consume a path-sized write reservation: {publication:?}"
    );
    drop(pressure_pin);
    drop(retained_pin);
}

#[test]
fn retention_expired_full_store_recovers_admission_and_reclaims_disk_without_losing_pinned_ancestry()
 {
    expired_full_store_recovery(48000);
}

#[test]
fn retention_expired_near_limit_records_recover_without_a_cap_increase() {
    expired_full_store_recovery(62000);
}

#[test]
fn retention_expiry_deletes_old_physical_rows_after_lowering_decode_bound() {
    lowered_decode_expiry_control(true);
}

#[test]
fn retention_existing_store_bootstraps_physical_bound_without_decoding_old_rows() {
    lowered_decode_expiry_control(false);
}

fn lowered_decode_expiry_control(has_bound: bool) {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.history_max_bytes = 65536;
    service.config.history_disk_bytes = 8 * 1024 * 1024;
    let start = 10_000_000;
    let mut old = Session {
        history_shared: true,
        history_namespace: Some("lowered-bound-old".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    old.ingest(&[json!({"role":"user","content":"x".repeat(62000)})])
        .unwrap();
    numeric_primary(&mut old, 0);
    at_decision_clock(&[start], || {
        history::committed(&service, "lowered-bound-old", &old, None).unwrap();
        let restored = history::restore(&service, "lowered-bound-old").unwrap();
        assert_eq!(restored.render_primary(), old.render_primary());
        assert_eq!(restored.primary.calls, 1);
        assert!((restored.primary.cost_usd - 0.002).abs() < 1e-12);
    });
    let key: String = rusqlite::Connection::open_with_flags(
        directory.path().join("history.index"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT key FROM records WHERE kind='nodes' AND length(data)>62000 AND length(data)<=65536",
        [],
        |row| row.get(0),
    )
    .unwrap();
    let frozen = at_decision_clock(&[start], || {
        history::accounting(&service, 0, "", 100).unwrap()["records"].clone()
    });
    let raw_exists = || {
        rusqlite::Connection::open_with_flags(
            directory.path().join("history.index"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM records WHERE kind='nodes' AND key=?1)",
            [&key],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    assert!(raw_exists(), "qualify a real legal near-limit physical row");
    if !has_bound {
        // A supported store written before the scalar existed must derive the
        // physical promise from SQL lengths, not try to decode the large row.
        rusqlite::Connection::open(directory.path().join("history.index"))
            .unwrap()
            .execute(
                "DELETE FROM records WHERE kind='metadata' AND key='physical_record_bound'",
                [],
            )
            .unwrap();
    }
    service.config.history_max_bytes = 8192;
    let mut small = Session {
        history_shared: true,
        history_namespace: Some("lowered-bound-new".into()),
        ..Session::default()
    };
    small
        .ingest(&[json!({"role":"user","content":"small new publication"})])
        .unwrap();
    at_decision_clock(&[start + 604799], || {
        history::committed(&service, "lowered-bound-new", &small, None).unwrap();
        assert!(raw_exists(), "decode reduction must not shorten retention");
    });
    at_decision_clock(&[start + 604800], || {
        for _ in 0..64 {
            history::committed(&service, "lowered-bound-new", &small, None).unwrap();
        }
        assert_eq!(
            history::accounting(&service, 0, "", 100).unwrap()["records"],
            frozen
        );
        assert_eq!(
            history::restore(&service, "lowered-bound-new")
                .unwrap()
                .render_primary(),
            small.render_primary()
        );
        assert!(
            !raw_exists(),
            "physical expiry must not inherit the smaller decode bound and strand an older legal row"
        );
    });
}

fn expired_full_store_recovery(source_bytes: usize) {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.history_max_bytes = 65536;
    service.config.history_max_nodes = 512;
    service.config.history_disk_bytes = 8 * 1024 * 1024;
    let start = 10_000_000;
    let bytes = || -> u64 {
        std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum()
    };
    let mut protected = Session {
        history_shared: true,
        history_namespace: Some("expiry-pinned".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    protected
        .ingest(&[json!({"role":"user","content":"pinned paid ancestry"})])
        .unwrap();
    numeric_primary(&mut protected, 0);
    let pin = at_decision_clock(&[start], || {
        save(&service, "expiry-pinned", &protected).unwrap();
        history::pin(&service, "expiry-pinned").unwrap()
    });
    let mut frozen = Vec::new();
    let before = at_decision_clock(&[start], || {
        frozen = history::accounting(&service, 0, "", 100).unwrap()["records"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(frozen.len(), 1);
        let mut rejected = false;
        let mut accepted = 0;
        for n in 0..256 {
            let mut pressure = Session {
                history_shared: true,
                history_namespace: Some("expiry-pressure".into()),
                ..Session::default()
            };
            pressure
                .ingest(&[
                    json!({"role":"user","content":format!("{n} {}", "x".repeat(source_bytes))}),
                ])
                .unwrap();
            match history::committed(&service, "expiry-pressure", &pressure, None) {
                Ok(()) => accepted += 1,
                Err(error) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
                    rejected = true;
                    break;
                }
            }
        }
        assert!(
            accepted > 0 && rejected,
            "qualify real unchanged-cap disk pressure"
        );
        assert!(bytes() <= service.config.history_disk_bytes);
        assert!(
            history::pin(&service, "expiry-new").is_err(),
            "qualify owner-admission pressure before expiry"
        );
        bytes()
    });
    at_decision_clock(&[start + 604799], || {
        assert!(history::pin(&service, "expiry-new").is_err());
        assert!(history::restore(&service, "expiry-pressure").is_ok());
    });
    at_decision_clock(&[start + 604800], || {
        // Admission itself must allow bounded maintenance to make progress: a
        // refused new owner cannot be required to publish the cleanup first.
        let mut recovered = None;
        for _ in 0..64 {
            if let Ok(pin) = history::pin(&service, "expiry-new") {
                recovered = Some(pin);
                break;
            }
        }
        let after_admission = bytes();
        let retained = history::restore(&service, "expiry-pinned").unwrap();
        assert_eq!(retained.render_primary(), protected.render_primary());
        assert_eq!(
            serde_json::to_value(&retained.primary).unwrap(),
            serde_json::to_value(&protected.primary).unwrap()
        );
        assert_eq!(
            history::accounting(&service, 0, "", 100).unwrap()["records"],
            json!(frozen)
        );
        assert!(
            recovered.is_some(),
            "seven-day expiry must recover admission without a cap increase or manual deletion; before={before}, after={}",
            bytes()
        );
        assert!(
            bytes() < before,
            "maintenance must physically reclaim expired pages"
        );
        let mut next = Session {
            history_shared: true,
            history_namespace: Some("expiry-new".into()),
            ..Session::default()
        };
        next.ingest(&[json!({"role":"user","content":"writable after expiry"})])
            .unwrap();
        let before_publication = bytes();
        let publication = history::committed(&service, "expiry-new", &next, None);
        let files = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.metadata().unwrap().len(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert!(
            publication.is_ok(),
            "expiry writable publication failed: source_bytes={source_bytes} before_pressure={before} after_admission={after_admission} before_publication={before_publication} files_after={files:?} publication={publication:?}"
        );
        assert_eq!(
            history::restore(&service, "expiry-new")
                .unwrap()
                .render_primary(),
            next.render_primary()
        );
        assert!(bytes() <= service.config.history_disk_bytes);
        // A fresh service must retain the paid owner too, not merely the live
        // decoded Session held by this fixture. Keep its pin through settlement.
        history::settled(&service, "expiry-pinned", &retained).unwrap();
        let mut restarted = checkpoint_service(directory.path());
        restarted.config.history_max_bytes = service.config.history_max_bytes;
        restarted.config.history_max_nodes = service.config.history_max_nodes;
        restarted.config.history_disk_bytes = service.config.history_disk_bytes;
        let restored = history::restore(&restarted, "expiry-pinned").unwrap();
        assert_eq!(restored.render_primary(), protected.render_primary());
        assert_eq!(
            serde_json::to_value(&restored.primary).unwrap(),
            serde_json::to_value(&protected.primary).unwrap()
        );
    });
    drop(pin);
}

#[test]
fn retention_shared_mirror_never_exceeds_physical_admission_before_sql() {
    external_checkpoint_admission_control(true);
}

#[test]
fn retention_explicit_checkpoint_never_exceeds_physical_admission_before_sql() {
    external_checkpoint_admission_control(false);
}

fn external_checkpoint_admission_control(shared: bool) {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.history_max_bytes = 65536;
    service.config.history_disk_bytes = 2 * 1024 * 1024;
    let owner = "external-admission";
    let mut session = Session {
        history_shared: shared,
        history_namespace: Some(owner.into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    session
        .ingest(&[json!({"role":"user","content":"frozen paid source"})])
        .unwrap();
    numeric_primary(&mut session, 0);
    let pin = history::pin(&service, owner).unwrap();
    save(&service, owner, &session).unwrap();
    let frozen = load_existing(&service, owner).unwrap();
    let accounting = history::accounting(&service, 0, "", 100).unwrap();
    let bytes = || -> u64 {
        std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum()
    };
    let mut peak = bytes();
    assert!(peak < service.config.history_disk_bytes);
    let mut candidate = frozen.clone();
    candidate.pending_output = vec![
        json!({"role":"assistant","content":"x".repeat(service.config.history_disk_bytes as usize + 4096)}),
    ];
    let publication = save_published(&service, owner, &candidate, |persisted, publication| {
        // Observe the real staged-file boundary before SQLite admission, not a
        // timer sample that can miss a transient peak or after-the-fact cleanup.
        peak = peak.max(bytes());
        publication.committed(&service, owner, persisted, None)
    });
    peak = peak.max(bytes());
    let restored = load_existing(&service, owner).unwrap();
    assert_eq!(restored.render_primary(), frozen.render_primary());
    assert_eq!(
        serde_json::to_value(&restored.primary).unwrap(),
        serde_json::to_value(&frozen.primary).unwrap()
    );
    assert_eq!(
        history::accounting(&service, 0, "", 100).unwrap(),
        accounting
    );
    assert!(
        publication.is_err() && peak <= service.config.history_disk_bytes,
        "unfit external checkpoint must be refused before exceeding admission: shared={shared} peak={peak} cap={} publication={publication:?}",
        service.config.history_disk_bytes
    );
    drop(pin);
}

#[test]
fn retention_numeric_queue_saturation_cannot_write_outside_physical_admission() {
    numeric_file_admission_control(false);
}

#[test]
fn retention_numeric_conflict_fork_cannot_write_outside_physical_admission() {
    numeric_file_admission_control(true);
}

fn numeric_file_admission_control(fork: bool) {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.history_max_bytes = 65536;
    service.config.history_disk_bytes = 2 * 1024 * 1024;
    let id = "numeric-physical-admission";
    let mut session = Session {
        history_shared: true,
        history_namespace: Some(id.into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    session
        .ingest(&[json!({"role":"user","content":"frozen numeric source"})])
        .unwrap();
    session.request_evidence.as_mut().unwrap().bind_archive(
        &history::FilePolicy::new(&service.config),
        id,
        session.history_namespace.as_deref(),
    );
    numeric_primary(&mut session, 0);
    let pin = history::pin(&service, id).unwrap();
    save(&service, id, &session).unwrap();
    let frozen = load_existing(&service, id).unwrap();
    let accounting = history::accounting(&service, 0, "", 100).unwrap();
    session = frozen.clone();
    let path = numeric_selected_path(directory.path(), &session);
    let mut original = std::fs::read(&path).unwrap();
    if fork {
        // A valid committed prefix followed by a conflicting abandoned tail
        // must be preserved, not overwritten when another branch is admitted.
        original.extend_from_slice(b"conflicting orphan\n");
        std::fs::write(&path, &original).unwrap();
    }
    let bytes = || -> u64 {
        std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum()
    };
    let pressure = private_file(&directory.path().join("pressure.fixture"), false).unwrap();
    pressure
        .set_len(service.config.history_disk_bytes - bytes() - 64)
        .unwrap();
    drop(pressure);
    let mut peak = bytes();
    assert!(peak < service.config.history_disk_bytes);
    let events = if fork { 1 } else { 4097 };
    for n in 1..=events {
        // Exercise the real receipt/queue producer, including its saturation
        // flush without invoking save, rather than calling an archive helper.
        numeric_primary(&mut session, n);
        peak = peak.max(bytes());
    }
    assert_eq!(session.primary.calls, events + 1);
    // Frozen catalog: 1000 ordinary (below 272000) input tokens at $2/million;
    // no writes, hits or output. The earlier $0.004 oracle was a tier mistake.
    assert!((session.primary.cost_usd - (events + 1) as f64 * 0.002).abs() < 1e-9);
    let mut fork_created = false;
    let publication = save_published(&service, id, &session, |persisted, publication| {
        // Capture the real archive/staging boundary while SQLite sidecars still
        // coexist, before the later writer close can remove them and hide peak.
        peak = peak.max(bytes());
        fork_created = std::fs::read_dir(directory.path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".request-evidence-")
            })
            .count()
            > 1;
        publication.committed(&service, id, persisted, None)
    });
    peak = peak.max(bytes());
    let restored = load_existing(&service, id).unwrap();
    assert_eq!(restored.render_primary(), frozen.render_primary());
    assert_eq!(
        serde_json::to_value(&restored.primary).unwrap(),
        serde_json::to_value(&frozen.primary).unwrap()
    );
    assert_eq!(
        history::accounting(&service, 0, "", 100).unwrap(),
        accounting
    );
    assert!(std::fs::read(&path).unwrap().starts_with(&original));
    assert!(
        publication.is_err() && peak <= service.config.history_disk_bytes,
        "numeric files must be admitted before append/fork/queue flush: fork={fork} fork_created={fork_created} peak={peak} cap={} publication={publication:?}",
        service.config.history_disk_bytes
    );
    drop(pin);
}

#[test]
fn retention_disk_admission_bounds_wal_and_preserves_prior_generation() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.history_disk_bytes = 1024 * 1024;
    let mut protected = Session {
        history_shared: true,
        history_namespace: Some("admission-protected".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    protected
        .ingest(&[json!({"role":"user","content":"paid protected source"})])
        .unwrap();
    numeric_primary(&mut protected, 0);
    save(&service, "protected", &protected).unwrap();
    let before = serde_json::to_value(history::restore(&service, "protected").unwrap()).unwrap();
    let old_generation = history::ReadView::new(&service).unwrap();
    let mut rejected = false;
    for n in 0..100 {
        let mut next = Session {
            history_shared: true,
            history_namespace: Some("disk-pressure".into()),
            ..Session::default()
        };
        next.ingest(&[
            json!({"role":"user","content":format!("{n} {}","numeric pressure ".repeat(256))}),
        ])
        .unwrap();
        let result = history::committed(&service, "pressure", &next, None);
        let bytes: u64 = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(
            bytes <= service.config.history_disk_bytes,
            "admission must include a retained reader's WAL/sidecars, not just database pages: {bytes}"
        );
        if let Err(error) = result {
            assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
            rejected = true;
            break;
        }
    }
    assert!(
        rejected,
        "disk pressure must reject explicitly, not evict promised history"
    );
    assert_eq!(
        serde_json::to_value(old_generation.restore("protected").unwrap()).unwrap(),
        before
    );
    drop(old_generation);
    drop(service);
    let service = checkpoint_service(directory.path());
    assert_eq!(
        serde_json::to_value(history::restore(&service, "protected").unwrap()).unwrap(),
        before
    );
}
#[test]
fn retention_numeric_accounting_is_independent_of_raw_history_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let start = 10_000_000;
    let id = "retained-accounting";
    let mut frozen = Vec::new();
    at_decision_clock(&[start], || {
        let service = checkpoint_service(directory.path());
        let mut session = Session {
            history_shared: true,
            history_namespace: Some("accounting-history".into()),
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        session
            .ingest(&[json!({"role":"user","content":"private raw sentinel"})])
            .unwrap();
        numeric_primary(&mut session, 0);
        save(&service, id, &session).unwrap();
        // Duplicate checkpoint saves are not additional money observations.
        save(&service, id, &session).unwrap();
        let db = rusqlite::Connection::open(directory.path().join("history.index")).unwrap();
        let mut statement = db
            .prepare(
                "SELECT data FROM records WHERE kind='accounting' AND owner=?1 ORDER BY at,key",
            )
            .unwrap();
        frozen = statement
            .query_map([id], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            frozen.len(),
            1,
            "a settled native receipt needs independent durable numeric accounting"
        );
        let value: Value = serde_json::from_slice(&frozen[0]).unwrap();
        assert_eq!(value["primary"]["calls"], 1);
        assert!((value["primary"]["cost_usd"].as_f64().unwrap() - 0.002).abs() < 1e-12);
        assert_eq!(
            value["estimate"]["traditional_cost"],
            serde_json::to_value(
                session
                    .request_evidence
                    .as_ref()
                    .unwrap()
                    .snapshot()
                    .dashboard(false)
            )
            .unwrap()["traditional_cost"]
        );
    });
    for elapsed in [604799, 604800, 120 * 86400] {
        at_decision_clock(&[start + elapsed], || {
            let service = checkpoint_service(directory.path());
            assert_eq!(history::restore(&service, id).is_ok(), elapsed < 604800);
            let mut other = Session {
                history_shared: true,
                history_namespace: Some("new-history".into()),
                ..Session::default()
            };
            other
                .ingest(&[json!({"role":"user","content":"unrelated housekeeping"})])
                .unwrap();
            history::committed(&service, "other", &other, None).unwrap();
            let db = rusqlite::Connection::open(directory.path().join("history.index")).unwrap();
            let data: Vec<u8> = db
                .query_row(
                    "SELECT data FROM records WHERE kind='accounting' AND owner=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                data, frozen[0],
                "months of old money/provenance must not be repriced or erased with raw history"
            );
            if elapsed >= 604800 {
                let rows: u64 = db
                    .query_row(
                        "SELECT count(*) FROM records WHERE kind='snapshots' AND key=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(
                    rows, 0,
                    "expired raw checkpoint must be collected independently of numeric evidence"
                );
            }
        });
    }
}
#[test]
fn retention_reference_pressure_preserves_inactive_owner_and_all_observed_ids() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.history_max_references = 4;
    let namespace = "retention-pressure";
    let mut frozen = None;
    for n in 0..20 {
        let id = if n == 0 { "inactive" } else { "busy" };
        let input = vec![json!({"role":"user","content":id})];
        let mut session = if n > 1 {
            history::restore(&service, id).unwrap()
        } else {
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(namespace.into()),
                ..Session::default()
            };
            session.ingest(&input).unwrap();
            session
        };
        session.completed_requests += 1;
        let response = json!({"id":format!("response-{n}"), "output":[
            {"role":"assistant","content":format!("observed-{n}")} ]});
        history::committed(&service, id, &session, Some((&input, &response))).unwrap();
        if n == 0 {
            frozen = Some(serde_json::to_value(&session).unwrap());
        }
    }
    drop(service);
    let service = checkpoint_service(directory.path());
    assert_eq!(
        serde_json::to_value(
            history::restore(&service, "inactive")
                .expect("seven-day inactive owner must survive reference pressure and restart")
        )
        .unwrap(),
        frozen.unwrap()
    );
    for n in 0..20 {
        let owner = if n == 0 { "inactive" } else { "busy" };
        let delta = json!({"role":"user","content":"resume"});
        let mut body = json!({"input":[delta],"previous_response_id":format!("response-{n}")});
        history::expand(&service, namespace, &mut body)
            .expect("every observed identifier inside retention remains expandable");
        assert_eq!(
            body["input"],
            json!([
            {"role":"user","content":owner},
            {"role":"assistant","content":format!("observed-{n}")}, delta])
        );
    }
}
