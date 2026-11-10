// Retention promises cannot be overridden by the continuation working-set cap.
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
