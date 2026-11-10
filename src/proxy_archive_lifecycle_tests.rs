// Actual request cardinality, not staged reference vectors. Exercise the
// production receipt extractor, Session checkpoint publisher and cold loader.
#[test]
fn archive_actual_over_limit_numeric_frontier_grows_through_session_restart() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let id = pr154_session_id("over-limit");
    let mut session = load(&service, &id).unwrap();
    session.request_evidence = Some(crate::proxy_savings::RequestEvidence::default());
    session.request_evidence.as_mut().unwrap().bind_archive(
        &history::FilePolicy::new(&service.config),
        &id,
        session.history_namespace.as_deref(),
    );
    let mut body = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
        "prompt_cache_options":{"mode":"implicit","ttl":"30m"},
        "input":(0..32769).map(|i|json!({"role":"user","content":format!("item {i}")})).collect::<Vec<_>>()});
    let mut expected = 0.0;
    for i in 0..4 {
        if i > 0 {
            body["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("growth {i}")}));
        }
        let input = 300000 + i * 100;
        let response = json!({"status":"completed","output":[],"usage":{"input_tokens":input,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}});
        let paid = session.primary.settle_with_rates(
            "gpt-6.1-sol",
            &response["usage"],
            true,
            receipt_rates("gpt-6.1-sol", &response["usage"], None, true),
        );
        session.request_evidence.as_mut().unwrap().primary_settled(
            crate::proxy_savings::PrimaryObservation {
                body: &body,
                response: &response,
                route: "route",
                at: 100 + i,
                standard: true,
                resolved: None,
                cost: paid,
                measurements: None,
            },
        );
        save(&service, &id, &session).unwrap();
        let restarted_service = checkpoint_service(directory.path());
        session = load(&restarted_service, &id).unwrap();
        let estimate = session.request_evidence.as_ref().unwrap().snapshot();
        assert_eq!(session.primary.calls, i + 1);
        assert!(
            (session.primary.cost_usd
                - (0..=i)
                    .map(|n| (300000 + n * 100) as f64 * 4e-6)
                    .sum::<f64>())
            .abs()
                < 1e-12
        );
        let incoming = 21000 + i * 100;
        expected += if i == 0 {
            incoming as f64 * 2.5e-6
        } else {
            (incoming - 100) as f64 * 0.1e-6 + 100.0 * 2.5e-6
        };
        let trajectory = serde_json::to_value(estimate.chronological.unwrap()).unwrap();
        assert_eq!(trajectory["primary_responses"], i + 1);
        assert_eq!(trajectory["initial_compactions"], 1);
        assert_eq!(trajectory["traditional_compactions"], 0);
        let actual = trajectory["traditional_cost"].as_f64().unwrap();
        assert!(
            (actual - expected).abs() < 1e-12,
            "R2 request {} actual={actual} independent_expected={expected}",
            i + 1
        );
    }
}

#[test]
fn archive_optional_conflict_fork_cannot_spend_paid_settlement_promise() {
    for shared in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.history_max_bytes = 65536;
        service.config.history_disk_bytes = 8 * 1024 * 1024;
        let id = pr154_session_id("settlement-fork-capacity");
        let mut session = Session {
            history_shared: shared,
            history_namespace: Some("settlement-fork-capacity".into()),
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        session
            .ingest(&[json!({"role":"user","content":"paid source"})])
            .unwrap();
        session.request_evidence.as_mut().unwrap().bind_archive(
            &history::FilePolicy::new(&service.config),
            &id,
            session.history_namespace.as_deref(),
        );
        // Grow a genuine committed numeric prefix larger than an emergency
        // transaction's startup, without manufacturing a watermark or receipt.
        let mut n = 0;
        loop {
            for _ in 0..8 {
                numeric_primary(&mut session, n);
                n += 1;
            }
            save(&service, &id, &session).unwrap();
            session = load(&service, &id).unwrap();
            let path = numeric_selected_path(directory.path(), &session);
            if std::fs::metadata(path).unwrap().len() >= 740000 {
                break;
            }
            assert!(n < 2000, "bounded fixture prefix construction");
        }
        let archive = numeric_selected_path(directory.path(), &session);
        let committed = std::fs::read(&archive).unwrap();
        assert!(
            committed.len() < 780000,
            "qualify fork fits only by spending the promise"
        );
        session.primary_pending = true;
        save(&service, &id, &session).unwrap();
        let _pin = history::pin(&service, &id).unwrap();
        let mut orphan = committed.clone();
        orphan.extend_from_slice(b"conflicting abandoned tail\n");
        std::fs::write(&archive, &orphan).unwrap();
        // Fill ONLY ordinary headroom, as measured by the real connected
        // physical owner. Its paid and maintenance promises remain untouched.
        let publication = history::FilePolicy::new(&service.config).acquire().unwrap();
        let mut low = 0;
        let mut high = service.config.history_disk_bytes;
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if publication.admit_file(directory.path(), mid).is_ok() {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        File::create(directory.path().join("pressure.bin"))
            .unwrap()
            .set_len(low)
            .unwrap();
        assert!(publication.admit_file(directory.path(), 1).is_err());
        drop(publication);
        let before = session.primary.cost_usd;
        numeric_primary(&mut session, n);
        session.primary_pending = false;
        let result = save_paid_accounting(&service, &id, &session, "paid-fork");
        assert!(
            result.is_ok(),
            "optional archive fork spent protected paid settlement: shared={shared}, result={result:?}"
        );
        let restored = load(&service, &id).unwrap();
        assert_eq!(restored.primary.calls, n + 1);
        assert!((restored.primary.cost_usd - before - 0.002).abs() < 1e-10);
        assert!(!restored.primary_pending);
        assert_eq!(std::fs::read(&archive).unwrap(), orphan);
        assert_eq!(numeric_selected_path(directory.path(), &restored), archive);
        assert!(
            !restored
                .request_evidence
                .as_ref()
                .unwrap()
                .snapshot()
                .archive_unavailable
        );
        let cold = checkpoint_service(directory.path());
        let restarted = load(&cold, &id).unwrap();
        assert_eq!(restarted.primary.calls, n + 1);
        assert_eq!(restarted.primary.cost_usd, restored.primary.cost_usd);
    }
}

// REVIEW-02: real synchronous reviewer and primary relay, faulted persistence.
#[tokio::test]
async fn archive_sync_reviewer_stale_reservation_and_primary_fault_preserve_committed_receipt() {
    let (url, mut captures, provider) = review_capture_provider().await;
    for primary in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.classifier_url = Some(url.clone());
        let service = Arc::new(service);
        let id = pr154_session_id("numeric-sync");
        let mut session = semantic_subject();
        session.history_shared = false;
        session.request_evidence = Some(crate::proxy_savings::RequestEvidence::default());
        session.request_evidence.as_mut().unwrap().bind_archive(
            &history::FilePolicy::new(&service.config),
            &id,
            session.history_namespace.as_deref(),
        );
        let projection = planning::Projection::canonical(&session);
        let mut run = ReviewRun::default(); // synchronous, not background settlement
        assert!(run_review(&service, &id, &mut session, &projection, &mut run).await);
        captures.recv().await.unwrap();
        assert_eq!(session.shadow.calls, 1);
        save(&service, &id, &session).unwrap();
        let before = load(&service, &id).unwrap();
        let estimate = before.request_evidence.as_ref().unwrap().snapshot();
        assert_eq!(estimate.coverage.reviewer_responses, 1);
        let archive = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".request-evidence-")
            })
            .unwrap();
        let committed = std::fs::read(&archive).unwrap();
        let mut reserved = session.clone(); // intentionally same production caller, not reloaded
        reserved.primary_pending = true;
        if primary {
            save(&service, &id, &reserved).unwrap();
        }
        std::fs::write(directory.path().join(".numeric-write-fault"), b"partial").unwrap();
        std::fs::create_dir(directory.path().join(format!(".{id}.tmp"))).unwrap();
        if primary {
            // Drive actual terminal settlement through the production response relay.
            let router = Router::new().route("/responses", axum::routing::post(|| async {
                axum::Json(json!({"id":"numeric-primary","status":"completed","output":[],
                    "usage":{"input_tokens":1000,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}}))
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
            let upstream_server =
                tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let upstream = service.client.post(endpoint).send().await.unwrap();
            let outbound = json!({"model":"gpt-6.1-sol","max_output_tokens":1,"input":session.render_primary()});
            let commit = Commit {
                service: service.clone(),
                id: id.clone(),
                request_id: "numeric-primary".into(),
                candidate: session.clone(),
                reviewed: session,
                submitted_ids: Vec::new(),
                observed_input: outbound["input"].as_array().unwrap().clone(),
                outbound,
                native: false,
                dispatched_at: now(),
                _history_pin: history::pin(&service, &id).unwrap(),
                _lock: session_lock(&service, &id).await,
            };
            let response = relay(upstream, Some(commit), None, false, None).await;
            assert!(
                axum::body::to_bytes(response.into_body(), 10000)
                    .await
                    .is_err()
            );
            upstream_server.abort();
        } else {
            assert!(save(&service, &id, &reserved).is_err());
        }
        // New service/loader is a genuine cold accounting reader of old authority.
        let restarted_service = checkpoint_service(directory.path());
        let restarted = load(&restarted_service, &id).unwrap();
        assert_eq!(restarted.shadow.calls, 1);
        assert_eq!(restarted.shadow.cost_usd, before.shadow.cost_usd);
        let after = restarted.request_evidence.as_ref().unwrap().snapshot();
        assert_eq!(
            after.coverage.reviewer_responses, 1,
            "R1 committed reviewer receipt lost, primary={primary}"
        );
        assert_eq!(
            after.observed_reviewer_cost,
            estimate.observed_reviewer_cost
        );
        assert!(
            !after.archive_unavailable,
            "R1 restart must read the committed numeric receipt"
        );
        assert!(std::fs::read(&archive).unwrap().starts_with(&committed));
    }
    provider.abort();
}

fn numeric_selected_path(directory: &Path, session: &Session) -> PathBuf {
    let value = serde_json::to_value(session.request_evidence.as_ref().unwrap()).unwrap();
    let mark = &value["archive"];
    let generation = mark["generation"]
        .as_str()
        .map_or(String::new(), |g| format!("-g{g}"));
    let namespace = Sha256::digest(serde_json::to_vec(&mark["namespace"]).unwrap());
    directory.join(format!(
        ".request-evidence-{}-{namespace:x}{generation}.jsonl",
        mark["owner"].as_str().unwrap(),
    ))
}

fn numeric_primary(session: &mut Session, n: u64) {
    let body = json!({"model":"gpt-6.1-sol","max_output_tokens":1,
        "input":[{"role":"user","content":"raw-body-storage-sentinel"}]});
    let response = json!({"status":"completed","output":[],
        "usage":{"input_tokens":1000,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}},
        "private":"raw-response-storage-sentinel"});
    let paid = session.primary.settle_with_rates(
        "gpt-6.1-sol",
        &response["usage"],
        true,
        receipt_rates("gpt-6.1-sol", &response["usage"], None, true),
    );
    session.request_evidence.as_mut().unwrap().primary_settled(
        crate::proxy_savings::PrimaryObservation {
            body: &body,
            response: &response,
            route: "route",
            at: 100 + n,
            standard: true,
            resolved: None,
            cost: paid,
            measurements: None,
        },
    );
    session.completed_requests += 1;
}

#[test]
fn archive_orphan_generation_publication_failure_keeps_canonical_cut_and_reuses_retry() {
    for shared in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        let id = pr154_session_id("orphan-fault");
        let mut session = semantic_subject();
        session.history_shared = shared;
        session.request_evidence = Some(crate::proxy_savings::RequestEvidence::default());
        session.request_evidence.as_mut().unwrap().bind_archive(
            &history::FilePolicy::new(&service.config),
            &id,
            session.history_namespace.as_deref(),
        );
        numeric_primary(&mut session, 0);
        save(&service, &id, &session).unwrap();
        session = load(&service, &id).unwrap();
        let path = numeric_selected_path(directory.path(), &session);
        let mut orphan = std::fs::read(&path).unwrap();
        orphan.extend_from_slice(b"conflicting orphan\n");
        std::fs::write(&path, &orphan).unwrap();
        numeric_primary(&mut session, 1);
        let fault = directory.path().join(if shared {
            ".history-index.tmp".into()
        } else {
            format!(".{id}.tmp")
        });
        if shared {
            history::fixture::checkpoint_fault(&directory.path().join("history.index"), true);
        } else {
            std::fs::create_dir(&fault).unwrap();
        }
        assert!(save(&service, &id, &session).is_err());
        let prior = load(&checkpoint_service(directory.path()), &id).unwrap();
        assert_eq!(prior.primary.calls, 1);
        assert_eq!(numeric_selected_path(directory.path(), &prior), path);
        assert_eq!(
            serde_json::to_value(
                prior
                    .request_evidence
                    .as_ref()
                    .unwrap()
                    .snapshot()
                    .chronological
                    .unwrap()
            )
            .unwrap()["primary_responses"]
                .as_u64()
                .unwrap(),
            1
        );
        assert_eq!(std::fs::read(&path).unwrap(), orphan);
        let branches = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".request-evidence-")
            })
            .count();
        assert_eq!(branches, 2); // complete orphan fork exists, not authority
        if shared {
            history::fixture::checkpoint_fault(&directory.path().join("history.index"), false);
        } else {
            std::fs::remove_dir(&fault).unwrap();
        }
        save(&service, &id, &session).unwrap();
        let current = load(&checkpoint_service(directory.path()), &id).unwrap();
        assert_eq!(current.primary.calls, 2);
        assert_eq!(
            serde_json::to_value(
                current
                    .request_evidence
                    .as_ref()
                    .unwrap()
                    .snapshot()
                    .chronological
                    .unwrap()
            )
            .unwrap()["primary_responses"]
                .as_u64()
                .unwrap(),
            2
        );
        assert_ne!(numeric_selected_path(directory.path(), &current), path);
        let retried = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".request-evidence-")
            })
            .count();
        assert_eq!(retried, branches);
        assert_eq!(std::fs::read(&path).unwrap(), orphan);
    }
}

#[tokio::test]
async fn archive_orphan_delayed_async_receipts_merge_into_fresh_generation_once() {
    for outcome in ["completed", "failed", "missing-usage"] {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let wait = gate.clone();
        let router = Router::new().route("/review", post(move |axum::Json(_body): axum::Json<Value>| {
            let wait = wait.clone();
            let sent = sent.clone();
            async move {
                sent.send(()).unwrap();
                let _permit = wait.acquire().await.unwrap();
                let mut value = json!({"status":if outcome == "failed" {"failed"} else {"completed"},
                    "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"protected\":[],\"removable\":[],\"memories\":[]}"}]}],
                    "usage":{"input_tokens":10000,"output_tokens":7,"input_tokens_details":{"cached_tokens":9000}}});
                if outcome == "missing-usage" { value.as_object_mut().unwrap().remove("usage"); }
                axum::Json(value)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/review", listener.local_addr().unwrap());
        let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.classifier_url = Some(url);
        let service = Arc::new(service);
        let id = pr154_session_id("async-orphan");
        let mut session = semantic_subject();
        session.history_shared = true;
        session.bootstrap_review_pending = true;
        session.request_evidence = Some(crate::proxy_savings::RequestEvidence::default());
        session.request_evidence.as_mut().unwrap().bind_archive(
            &history::FilePolicy::new(&service.config),
            &id,
            session.history_namespace.as_deref(),
        );
        numeric_primary(&mut session, 0);
        save(&service, &id, &session).unwrap();
        session = load(&service, &id).unwrap();
        let projection = planning::Projection::canonical(&session);
        dispatch_review_for(
            service.clone(),
            id.clone(),
            &mut session,
            projection,
            "async-orphan".into(),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), received.recv())
            .await
            .unwrap()
            .unwrap();
        let _lock = session_lock(&service, &id).await;
        let mut current = load(&service, &id).unwrap();
        let old = numeric_selected_path(directory.path(), &current);
        let mut orphan = std::fs::read(&old).unwrap();
        orphan.extend_from_slice(b"async conflicting orphan\n");
        std::fs::write(&old, &orphan).unwrap();
        numeric_primary(&mut current, 1);
        save(&service, &id, &current).unwrap();
        let fresh = load(&service, &id).unwrap();
        let selected = numeric_selected_path(directory.path(), &fresh);
        assert_ne!(selected, old);
        gate.add_permits(1);
        drop(_lock);
        tokio::time::timeout(Duration::from_secs(10), async {
            while service.background_reviews.lock().await.contains(&id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let after = load(&checkpoint_service(directory.path()), &id).unwrap();
        assert_eq!(after.primary.calls, 2);
        assert_eq!(after.primary.cost_usd, fresh.primary.cost_usd);
        assert_eq!(after.shadow.calls, 1, "{outcome}");
        assert_eq!(
            after.shadow.unavailable_cost_calls,
            u64::from(outcome == "missing-usage")
        );
        let snapshot = after.request_evidence.as_ref().unwrap().snapshot();
        assert!(!snapshot.archive_unavailable, "{outcome}");
        assert_eq!(
            serde_json::to_value(snapshot.chronological.unwrap()).unwrap()["primary_responses"]
                .as_u64()
                .unwrap(),
            2
        );
        assert_eq!(numeric_selected_path(directory.path(), &after), selected);
        assert_eq!(std::fs::read(&old).unwrap(), orphan);
        let again = load(&checkpoint_service(directory.path()), &id).unwrap();
        assert_eq!(again.shadow.calls, 1);
        provider.abort();
    }
}

#[test]
fn archive_orphan_genuine_capture_beyond_pending_bound_and_cold_authority_restart() {
    for shared in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        let id = pr154_session_id("orphan-continuation");
        let mut session = load(&service, &id).unwrap();
        session.history_shared = shared;
        session.history_namespace = Some("fixture-orphan".into());
        session.request_evidence = Some(crate::proxy_savings::RequestEvidence::default());
        session.request_evidence.as_mut().unwrap().bind_archive(
            &history::FilePolicy::new(&service.config),
            &id,
            session.history_namespace.as_deref(),
        );
        numeric_primary(&mut session, 0);
        save(&service, &id, &session).unwrap();
        session = load(&service, &id).unwrap();
        let original = numeric_selected_path(directory.path(), &session);
        let mut orphan = std::fs::read(&original).unwrap();
        orphan.extend_from_slice(b"orphan bytes must not be counted\n");
        std::fs::write(&original, &orphan).unwrap();
        // Genuine production receipt extraction and paid settlement, not staged
        // event counts. Continue beyond 4096 receipts without a save: the real
        // byte bound may flush before the cardinality bound does.
        for n in 1..=4097 {
            numeric_primary(&mut session, n);
        }
        let evidence = serde_json::to_value(session.request_evidence.as_ref().unwrap()).unwrap();
        let durable = evidence["archive"]["events"].as_u64().unwrap();
        let pending = evidence["numeric_pending"].as_array().map_or(0, Vec::len);
        assert!(durable > 1, "conflict must not stall the original journal");
        assert!(pending < 4096);
        assert_eq!(
            durable + pending as u64,
            4098,
            "every genuine receipt conserved"
        );
        assert!(!evidence["numeric_lost"].as_bool().unwrap_or(false));
        assert_eq!(session.primary.calls, 4098);
        assert!((session.primary.cost_usd - 4098.0 * 0.002).abs() < 1e-9);
        save(&service, &id, &session).unwrap();
        let cold = checkpoint_service(directory.path());
        let mut restarted = load(&cold, &id).unwrap();
        let selected = numeric_selected_path(directory.path(), &restarted);
        assert_ne!(selected, original);
        assert_eq!(std::fs::read(&original).unwrap(), orphan);
        let estimate = restarted.request_evidence.as_ref().unwrap().snapshot();
        assert!(!estimate.archive_unavailable);
        assert_eq!(serde_json::to_value(estimate.chronological.as_ref().unwrap()).unwrap()["primary_responses"].as_u64().unwrap(), 4098);
        assert!((serde_json::to_value(estimate.chronological.as_ref().unwrap()).unwrap()["traditional_cost"].as_f64().unwrap() - 4098.0*0.002).abs() < 1e-9);
        let after = serde_json::to_value(restarted.request_evidence.as_ref().unwrap()).unwrap();
        assert!(after.get("numeric_pending").is_none());
        assert_eq!(after["archive"]["events"], 4098);
        // Shared indexed authority, not its diagnostic mirror, selects the fork.
        if shared {
            let index: Value = serde_json::from_slice(&history::fixture::read_index(
                &directory.path().join("history.index"),
            ))
            .unwrap();
            assert_eq!(
                index["snapshots"][&id]["request_evidence"]["archive"],
                after["archive"]
            );
        }
        numeric_primary(&mut restarted, 4098);
        save(&cold, &id, &restarted).unwrap();
        let final_session = load(&checkpoint_service(directory.path()), &id).unwrap();
        assert_eq!(final_session.primary.calls, 4099);
        assert_eq!(
            serde_json::to_value(
                final_session
                    .request_evidence
                    .as_ref()
                    .unwrap()
                    .snapshot()
                    .chronological
                    .unwrap()
            )
            .unwrap()["primary_responses"]
                .as_u64()
                .unwrap(),
            4099
        );
        assert_eq!(
            numeric_selected_path(directory.path(), &final_session),
            selected
        );
        assert_eq!(std::fs::read(&original).unwrap(), orphan);
        let bytes = std::fs::read(&selected).unwrap();
        for sentinel in [
            b"raw-body-storage-sentinel".as_slice(),
            b"raw-response-storage-sentinel".as_slice(),
        ] {
            assert!(!bytes.windows(sentinel.len()).any(|w| w == sentinel));
        }
    }
}
