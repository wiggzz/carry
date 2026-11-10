// Retention promises cannot be overridden by the continuation working-set cap.
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
