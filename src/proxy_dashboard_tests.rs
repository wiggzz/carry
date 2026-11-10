#[test]
fn dashboard_month_window_exposes_missing_decreasing_and_backward_boundaries() {
    for case in ["missing", "decreasing", "backward"] {
        let directory = tempfile::tempdir().unwrap();
        let start = 20_000_000;
        let end = start + 30 * 86400;
        let id = format!("{:064x}", 933);
        let mut session = Session {
            history_shared: true,
            history_namespace: Some(format!("boundary-{case}")),
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        session
            .ingest(&[json!({"role":"user","content":"private boundary source"})])
            .unwrap();
        at_decision_clock(
            &[if case == "missing" {
                start + 1
            } else {
                start - 1
            }],
            || {
                numeric_primary(&mut session, 0);
                save(&checkpoint_service(directory.path()), &id, &session).unwrap();
            },
        );
        at_decision_clock(&[start + 100], || {
            numeric_primary(&mut session, 1);
            save(&checkpoint_service(directory.path()), &id, &session).unwrap();
        });
        at_decision_clock(
            &[if case == "backward" {
                start + 50
            } else {
                start + 200
            }],
            || {
                numeric_primary(&mut session, 2);
                if case == "decreasing" {
                    // A staged incompatible native cumulative ledger, not a provider receipt.
                    session.primary.cost_usd = 0.001;
                }
                save(&checkpoint_service(directory.path()), &id, &session).unwrap();
            },
        );
        at_decision_clock(&[end + 60 * 86400], || {
            let service = checkpoint_service(directory.path());
            let frozen = history::accounting(&service, 0, "", 100).unwrap();
            assert_eq!(frozen["records"].as_array().unwrap().len(), 3);
            assert!(history::restore(&service, &id).is_err());
            let bytes = std::fs::read(directory.path().join("history.index")).unwrap();
            let report = history::accounting_window(&service, start, end).unwrap();
            assert_eq!(report["coverage"]["observed_owners"], 1, "{case}: {report}");
            assert_eq!(report["coverage"]["included_owners"], 0, "{case}: {report}");
            let reason = match case {
                "missing" => "missing_opening_owners",
                "decreasing" => "incompatible_owners",
                _ => "ambiguous_clock_owners",
            };
            assert_eq!(report["coverage"][reason], 1, "{case}: {report}");
            assert_eq!(report["usage"]["unavailable_owners"], 1);
            assert_eq!(report["comparison"]["unavailable_owners"], 1);
            assert!(report["comparison"]["carry_cost"].is_null());
            assert!(report["comparison"]["traditional_cost"].is_null());
            assert_eq!(
                history::accounting_window(&checkpoint_service(directory.path()), start, end)
                    .unwrap(),
                report
            );
            assert_eq!(history::accounting(&service, 0, "", 100).unwrap(), frozen);
            assert_eq!(
                std::fs::read(directory.path().join("history.index")).unwrap(),
                bytes
            );
        });
    }
}

#[test]
fn dashboard_month_window_separates_unknown_failed_attempts_from_successful_models() {
    let directory = tempfile::tempdir().unwrap();
    let start = 20_000_000;
    let end = start + 30 * 86400;
    let id = format!("{:064x}", 934);
    let mut session = Session {
        history_shared: true,
        history_namespace: Some("unknown-month-native".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    session
        .ingest(&[json!({"role":"user","content":"private native source"})])
        .unwrap();
    at_decision_clock(&[start - 1], || {
        numeric_primary(&mut session, 0);
        save(&checkpoint_service(directory.path()), &id, &session).unwrap();
    });
    at_decision_clock(&[start + 100], || {
        // Actual ledger settlement seam: an attempted failed call has no native usage.
        assert!(
            session
                .primary
                .settle_with_rates("unknown-month-price", &Value::Null, true, None)
                .is_none()
        );
        session.failed_primaries += 1;
        save(&checkpoint_service(directory.path()), &id, &session).unwrap();
    });
    at_decision_clock(&[end], || {
        numeric_primary(&mut session, 1);
        save(&checkpoint_service(directory.path()), &id, &session).unwrap();
    });
    at_decision_clock(&[end + 60 * 86400], || {
        let service = checkpoint_service(directory.path());
        let frozen = history::accounting(&service, 0, "", 100).unwrap();
        assert_eq!(frozen["records"].as_array().unwrap().len(), 3);
        assert!(history::restore(&service, &id).is_err());
        let bytes = std::fs::read(directory.path().join("history.index")).unwrap();
        let report = history::accounting_window(&service, start, end).unwrap();
        assert_eq!(report["coverage"]["included_owners"], 1);
        assert_eq!(report["usage"]["primary"]["calls"], 2);
        assert_eq!(report["usage"]["primary"]["unavailable_cost_calls"], 1);
        assert!((report["usage"]["primary"]["cost_usd"].as_f64().unwrap() - 0.002).abs() < 1e-12);
        assert_eq!(report["completed_requests"], 1);
        assert_eq!(report["failed_primaries"], 1);
        assert_eq!(report["comparison"]["included_owners"], 1);
        assert_eq!(report["comparison"]["primary_responses"], 1);
        assert!((report["comparison"]["carry_cost"].as_f64().unwrap() - 0.002).abs() < 1e-12);
        assert!((report["comparison"]["traditional_cost"].as_f64().unwrap() - 0.002).abs() < 1e-12);
        assert_eq!(
            history::accounting_window(&checkpoint_service(directory.path()), start, end).unwrap(),
            report
        );
        assert_eq!(history::accounting(&service, 0, "", 100).unwrap(), frozen);
        assert_eq!(
            std::fs::read(directory.path().join("history.index")).unwrap(),
            bytes
        );
    });
}

#[test]
fn dashboard_month_window_keeps_evidence_corrections_out_of_comparison_spend() {
    let directory = tempfile::tempdir().unwrap();
    let start = 20_000_000;
    let end = start + 30 * 86400;
    let current_id = format!("{:064x}", 931);
    let corrected_id = format!("{:064x}", 932);
    let mut current = Session {
        history_shared: true,
        history_namespace: Some("compatible-month-model".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    current
        .ingest(&[json!({"role":"user","content":"private monthly model source"})])
        .unwrap();
    let mut original = Value::Null;
    let mut original_session = None;
    at_decision_clock(&[start - 1], || {
        let service = checkpoint_service(directory.path());
        numeric_primary(&mut current, 0);
        save(&service, &current_id, &current).unwrap();
        let historical = historical_retention_fixture(&service, &corrected_id, false);
        save(&service, &corrected_id, &historical).unwrap();
        original_session = Some(historical);
        original = history::accounting(&service, 0, "", 100).unwrap()["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["owner"] == corrected_id)
            .unwrap()
            .clone();
        assert!(original["estimate"]["whole_window"].is_null());
    });
    let mut corrected = Value::Null;
    at_decision_clock(&[start + 15 * 86400], || {
        let service = checkpoint_service(directory.path());
        historical_retention_fixture(&service, &corrected_id, true);
        // The old raw owner has expired. Rebind the independently validated
        // historical prefix against the original saved session seam, not a
        // fabricated opening balance or a newly dispatched paid request.
        let historical =
            recover_success_prefix(&service, &corrected_id, original_session.take().unwrap())
                .unwrap();
        save(&service, &corrected_id, &historical).unwrap();
        numeric_primary(&mut current, 1);
        save(&service, &current_id, &current).unwrap();
        corrected = history::accounting(&service, 0, "", 100).unwrap()["records"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|r| r["owner"] == corrected_id)
            .unwrap()
            .clone();
        assert_eq!(corrected["primary"], original["primary"]);
        assert_eq!(corrected["reviewer"], original["reviewer"]);
        assert_eq!(
            corrected["completed_requests"],
            original["completed_requests"]
        );
        assert_eq!(corrected["estimate"]["observed_primary_cost"], 0.002);
        assert_ne!(
            corrected["numeric_provenance"],
            original["numeric_provenance"]
        );
        save(&service, &corrected_id, &historical).unwrap();
    });
    at_decision_clock(&[end], || {
        let service = checkpoint_service(directory.path());
        numeric_primary(&mut current, 2);
        save(&service, &current_id, &current).unwrap();
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    at_decision_clock(&[end + 60 * 86400], || {
        let service = checkpoint_service(directory.path());
        let frozen = history::accounting(&service, 0, "", 100).unwrap()["records"].clone();
        assert_eq!(frozen.as_array().unwrap().len(), 5);
        assert!(frozen.as_array().unwrap().contains(&original));
        assert!(frozen.as_array().unwrap().contains(&corrected));
        assert!(history::restore(&service, &current_id).is_err());
        assert!(history::restore(&service, &corrected_id).is_err());
        let authority = std::fs::read(directory.path().join("history.index")).unwrap();
        let mut reports = Vec::new();
        for _ in 0..2 {
            let restarted = Arc::new(checkpoint_service(directory.path()));
            let report = runtime.block_on(async {
                let response = dashboard_accounting(
                    State(restarted.clone()),
                    ConnectInfo("127.0.0.1:1".parse().unwrap()),
                    HeaderMap::new(),
                    axum::extract::Query(
                        serde_json::from_value(json!({"start_at":start,"end_at":end})).unwrap(),
                    ),
                )
                .await
                .unwrap();
                let bytes = axum::body::to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap();
                serde_json::from_slice::<Value>(&bytes).unwrap()
            });
            assert_eq!(
                history::accounting(&restarted, 0, "", 100).unwrap()["records"],
                frozen
            );
            assert_eq!(
                std::fs::read(directory.path().join("history.index")).unwrap(),
                authority
            );
            assert_eq!(report["window"]["coverage"]["included_owners"], 2);
            assert_eq!(report["window"]["usage"]["primary"]["calls"], 2);
            assert!(
                (report["window"]["usage"]["primary"]["cost_usd"]
                    .as_f64()
                    .unwrap()
                    - 0.004)
                    .abs()
                    < 1e-12
            );
            reports.push(report);
        }
        assert_eq!(reports[0], reports[1]);
        let comparison = &reports[0]["window"]["comparison"];
        assert_eq!(
            comparison["included_owners"], 1,
            "historical model contributions need compatible boundaries; corrected old evidence is not new monthly spend: {}",
            reports[0]
        );
        assert_eq!(comparison["unavailable_owners"], 1);
        assert_eq!(comparison["primary_responses"], 2);
        assert!((comparison["carry_cost"].as_f64().unwrap() - 0.004).abs() < 1e-12);
        assert!((comparison["traditional_cost"].as_f64().unwrap() - 0.004).abs() < 1e-12);
        assert!(comparison["savings"].as_f64().unwrap().abs() < 1e-12);
    });
}

#[test]
fn dashboard_month_window_subtracts_owner_boundaries_after_raw_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let start = 20_000_000;
    let end = start + 30 * 86400;
    let ids = [format!("{:064x}", 921), format!("{:064x}", 922)];
    let mut sessions = ids
        .iter()
        .map(|id| {
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(format!("month-window-{id}")),
                request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
                ..Session::default()
            };
            session
                .ingest(&[json!({"role":"user","content":"month private source"})])
                .unwrap();
            session
        })
        .collect::<Vec<_>>();
    // The interval is (start,end], by recorded observation time. There are
    // multiple nonadditive cumulative snapshots on both sides of its bounds.
    let stages = [
        (start - 1, [2, 1]),
        (start, [1, 1]),
        (start + 15 * 86400, [1, 2]),
        (end, [1, 1]),
        (end + 1, [2, 1]),
    ];
    for (at, counts) in stages {
        at_decision_clock(&[at], || {
            let service = checkpoint_service(directory.path());
            for (n, (id, session)) in ids.iter().zip(sessions.iter_mut()).enumerate() {
                for _ in 0..counts[n] {
                    let turn = session.completed_requests;
                    numeric_primary(session, turn);
                    if at == end && session.shadow.calls == 0 {
                        let usage = json!({"input_tokens":1000,"output_tokens":0,
                            "input_tokens_details":{"cached_tokens":0}});
                        session.shadow.observe_with_rates(
                            "gpt-6.1-sol",
                            &usage,
                            true,
                            if n == 0 {
                                receipt_rates("gpt-6.1-sol", &usage, None, true)
                            } else {
                                None
                            },
                        );
                    }
                    save(&service, id, session).unwrap();
                    save(&service, id, session).unwrap();
                }
                let restored = history::restore(&service, id).unwrap();
                assert_eq!(restored.primary.calls, session.primary.calls);
                assert_eq!(restored.primary.cost_usd, session.primary.cost_usd);
            }
        });
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    at_decision_clock(&[end + 60 * 86400], || {
        let service = checkpoint_service(directory.path());
        let frozen = history::accounting(&service, 0, "", 100).unwrap()["records"].clone();
        assert_eq!(frozen.as_array().unwrap().len(), 13);
        for (id, session) in ids.iter().zip(&sessions) {
            assert!(
                history::restore(&service, id).is_err(),
                "raw retention expired independently"
            );
            let rows = frozen
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["owner"] == *id)
                .collect::<Vec<_>>();
            let last = rows.last().unwrap();
            assert_eq!(
                last["primary"],
                serde_json::to_value(&session.primary).unwrap()
            );
            assert_eq!(
                last["reviewer"],
                serde_json::to_value(&session.shadow).unwrap()
            );
            assert_eq!(
                last["estimate"],
                serde_json::to_value(
                    session
                        .request_evidence
                        .as_ref()
                        .unwrap()
                        .snapshot()
                        .dashboard(false)
                )
                .unwrap()
            );
            assert!(!last["numeric_provenance"].is_null());
        }
        let authority = std::fs::read(directory.path().join("history.index")).unwrap();
        let mut reports = Vec::new();
        for _ in 0..2 {
            let restarted = Arc::new(checkpoint_service(directory.path()));
            let report = runtime.block_on(async {
                let response = dashboard_accounting(
                    State(restarted.clone()),
                    ConnectInfo("127.0.0.1:1".parse().unwrap()),
                    HeaderMap::new(),
                    axum::extract::Query(
                        serde_json::from_value(json!({"start_at":start,"end_at":end,"limit":1}))
                            .unwrap(),
                    ),
                )
                .await
                .unwrap();
                assert_eq!(response.headers()["cache-control"], "no-store");
                let bytes = axum::body::to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap();
                serde_json::from_slice::<Value>(&bytes).unwrap()
            });
            assert_eq!(
                history::accounting(&restarted, 0, "", 100).unwrap()["records"],
                frozen
            );
            assert_eq!(
                std::fs::read(directory.path().join("history.index")).unwrap(),
                authority
            );
            reports.push(report);
        }
        assert_eq!(
            reports[0], reports[1],
            "restart and repeat are read-only and idempotent"
        );
        let window = &reports[0]["window"];
        assert_eq!(
            window["coverage"]["included_owners"], 2,
            "month reporting needs independently differenced owner contributions, not a latest lifetime subtotal: {}",
            reports[0]
        );
        assert_eq!(window["start_at"], start);
        assert_eq!(window["end_at"], end);
        assert_eq!(window["usage"]["primary"]["calls"], 5);
        assert_eq!(window["usage"]["primary"]["input_tokens"], 5000);
        assert_eq!(window["usage"]["primary"]["unavailable_cost_calls"], 0);
        assert!((window["usage"]["primary"]["cost_usd"].as_f64().unwrap() - 0.010).abs() < 1e-12);
        assert_eq!(window["usage"]["reviewer"]["calls"], 2);
        assert_eq!(window["usage"]["reviewer"]["input_tokens"], 2000);
        assert_eq!(window["usage"]["reviewer"]["unavailable_cost_calls"], 1);
        assert!((window["usage"]["reviewer"]["cost_usd"].as_f64().unwrap() - 0.002).abs() < 1e-12);
        assert_eq!(window["completed_requests"], 5);
        assert_eq!(window["failed_primaries"], 0);
    });
}

#[tokio::test]
async fn dashboard_session_pages_bound_serialized_bytes_and_keep_cursor_progress() {
    let directory = tempfile::tempdir().unwrap();
    let ids = (0..110).map(|n| format!("{n:064x}")).collect::<Vec<_>>();
    let mut config_service = checkpoint_service(directory.path());
    config_service.config.history_max_bytes = 65536;
    let service = Arc::new(config_service);
    for (n, id) in ids.iter().enumerate() {
        let mut session = Session {
            history_shared: true,
            history_namespace: Some(format!("byte-bounded-dashboard-{id}")),
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        session
            .ingest(&[json!({"role":"user","content":format!("retained page owner {n}")})])
            .unwrap();
        numeric_primary(&mut session, 0);
        save(&service, id, &session).unwrap();
        let restored = history::restore(&service, id).unwrap();
        assert_eq!(restored.primary.calls, 1);
        assert_eq!(restored.history.len(), 1);
        assert!((restored.primary.cost_usd - 0.002).abs() < 1e-12);
    }
    let frozen = std::fs::read(directory.path().join("history.index")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/carry/dashboard/stats", get(dashboard_stats))
        .with_state(service.clone());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::new();
    let url = format!("http://{address}/carry/dashboard/stats");
    // Qualify actual HTTP query limits and cursor rejection before grading the
    // new byte bound. These already-correct controls are not defect REDs.
    for query in ["limit=0", "limit=1"] {
        let report: Value = client
            .get(format!("{url}?{query}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(report["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(report["sessions"][0]["id"], ids[0]);
        assert_eq!(report["usage"]["primary"]["calls"], 110);
        assert!((report["usage"]["primary"]["cost_usd"].as_f64().unwrap() - 0.22).abs() < 1e-12);
    }
    for query in ["after_id=bad", "limit=invalid", "limit=-1"] {
        assert_eq!(
            client
                .get(format!("{url}?{query}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let mut first_rows = None;
    for query in ["", "limit=100", "limit=999999"] {
        let bytes = client
            .get(format!("{url}?{query}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report["usage"]["primary"]["calls"], 110);
        assert!((report["usage"]["primary"]["cost_usd"].as_f64().unwrap() - 0.22).abs() < 1e-12);
        assert_eq!(report["unavailable_sessions"], 0);
        let rows = report["sessions"].as_array().unwrap();
        assert!(!rows.is_empty() && rows.len() <= 100);
        assert_eq!(report["session_page"]["has_more"], true);
        assert_eq!(
            report["session_page"]["next_after"],
            rows.last().unwrap()["id"]
        );
        assert_eq!(
            std::fs::read(directory.path().join("history.index")).unwrap(),
            frozen
        );
        assert!(
            bytes.len() <= 65536,
            "serialized dashboard page exceeds its working-set bound: {} bytes for {} rows",
            bytes.len(),
            rows.len()
        );
        if let Some(expected) = &first_rows {
            assert_eq!(rows, expected);
        } else {
            first_rows = Some(rows.clone());
        }
    }
    let mut after = String::new();
    let mut seen = Vec::new();
    for _ in 0..ids.len() {
        let bytes = client
            .get(&url)
            .query(&[("after_id", &after)])
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert!(bytes.len() <= 65536);
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report["usage"]["primary"]["calls"], 110);
        assert!((report["usage"]["primary"]["cost_usd"].as_f64().unwrap() - 0.22).abs() < 1e-12);
        for row in report["sessions"].as_array().unwrap() {
            seen.push(row["id"].as_str().unwrap().to_owned());
        }
        if report["session_page"]["has_more"] == false {
            assert!(report["session_page"]["next_after"].is_null());
            break;
        }
        let next = report["session_page"]["next_after"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            next > after,
            "byte-limited pagination must make strict keyset progress"
        );
        assert_eq!(Some(&next), seen.last());
        after = next;
    }
    server.abort();
    assert_eq!(
        seen, ids,
        "byte paging must neither lose nor duplicate retained owners"
    );
    assert_eq!(
        std::fs::read(directory.path().join("history.index")).unwrap(),
        frozen
    );
}

#[test]
fn dashboard_observed_money_survives_raw_expiry_without_summing_cumulative_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let start = 10_000_000;
    let ids = [format!("{:064x}", 901), format!("{:064x}", 902)];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let poll = |service: Arc<Service>| {
        runtime.block_on(async {
            let response = dashboard_stats(
                State(service),
                ConnectInfo("127.0.0.1:1".parse().unwrap()),
                HeaderMap::new(),
                axum::extract::Query(SessionPage::default()),
            )
            .await
            .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        })
    };
    let mut frozen = Value::Null;
    at_decision_clock(&[start], || {
        let service = Arc::new(checkpoint_service(directory.path()));
        for (id, calls) in ids.iter().zip([2, 1]) {
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(format!("dashboard-accounting-{id}")),
                request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
                ..Session::default()
            };
            session
                .ingest(&[json!({"role":"user","content":"raw historical dashboard source"})])
                .unwrap();
            for n in 0..calls {
                numeric_primary(&mut session, n);
                save(&service, id, &session).unwrap();
            }
            let restored = history::restore(&service, id).unwrap();
            assert_eq!(restored.primary.calls, calls);
            assert!((restored.primary.cost_usd - 0.002 * calls as f64).abs() < 1e-12);
        }
        frozen = history::accounting(&service, 0, "", 100).unwrap()["records"].clone();
        assert_eq!(frozen.as_array().unwrap().len(), 3);
        let stats = poll(service);
        assert_eq!(stats["sessions"].as_array().unwrap().len(), 2);
        assert!(
            (stats["cost_estimate"]["observed_primary_cost"]
                .as_f64()
                .unwrap()
                - 0.006)
                .abs()
                < 1e-12,
            "qualify dashboard native-money totals before raw expiry: {stats}"
        );
    });
    at_decision_clock(&[start + 120 * 86400], || {
        let service = checkpoint_service(directory.path());
        let mut housekeeping = Session {
            history_shared: true,
            history_namespace: Some("dashboard-housekeeping".into()),
            ..Session::default()
        };
        housekeeping
            .ingest(&[json!({"role":"user","content":"unpaid maintenance"})])
            .unwrap();
        history::committed(&service, "housekeeping", &housekeeping, None).unwrap();
        for id in &ids {
            assert!(history::restore(&service, id).is_err());
            assert!(!directory.path().join(format!("{id}.json")).exists());
        }
        assert_eq!(
            history::accounting(&service, 0, "", 100).unwrap()["records"],
            frozen,
            "raw expiry must preserve every frozen numeric observation"
        );
        drop(service);
        let restarted = Arc::new(checkpoint_service(directory.path()));
        let stats = poll(restarted.clone());
        assert!(stats["sessions"].as_array().unwrap().is_empty());
        assert_eq!(
            history::accounting(&restarted, 0, "", 100).unwrap()["records"],
            frozen,
            "a dashboard poll cannot rewrite or reprice historical evidence"
        );
        assert!(
            stats["cost_estimate"]["observed_primary_cost"]
                .as_f64()
                .is_some_and(|money| (money - 0.006).abs() < 1e-12),
            "visible retained native money must use each owner's latest observation, not disappear with raw sessions or sum cumulative snapshots: {stats}"
        );
    });
}

#[tokio::test]
async fn dashboard_session_pages_keep_population_money_independent_of_raw_page() {
    let directory = tempfile::tempdir().unwrap();
    let ids = (0..7).map(|n| format!("{n:064x}")).collect::<Vec<_>>();
    {
        let service = checkpoint_service(directory.path());
        for (n, id) in ids.iter().enumerate() {
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(format!("paged-dashboard-{id}")),
                request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
                ..Session::default()
            };
            session
                .ingest(&[json!({"role":"user","content":format!("owned page source {n}")})])
                .unwrap();
            numeric_primary(&mut session, 0);
            save(&service, id, &session).unwrap();
            if n == 0 {
                numeric_primary(&mut session, 1);
                save(&service, id, &session).unwrap();
            }
        }
    }
    // Reopen the durable authority rather than relying on an in-memory owner
    // list. Repeated cumulative snapshots must not inflate population money.
    let service = Arc::new(checkpoint_service(directory.path()));
    let frozen = history::accounting(&service, 0, "", 100).unwrap()["records"].clone();
    assert_eq!(frozen.as_array().unwrap().len(), 8);
    for (n, id) in ids.iter().enumerate() {
        let session = history::restore(&service, id).unwrap();
        assert_eq!(session.primary.calls, if n == 0 { 2 } else { 1 });
        assert_eq!(session.history.len(), 1);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/carry/dashboard/stats", get(dashboard_stats))
        .with_state(service.clone());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::new();
    let mut after = String::new();
    let mut seen = Vec::new();
    for page_number in 0..4 {
        let bytes = client
            .get(format!("http://{address}/carry/dashboard/stats"))
            .query(&[("limit", "2"), ("after_id", after.as_str())])
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let stats: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            stats["cost_estimate"]["observed_primary_cost"]
                .as_f64()
                .is_some_and(|money| (money - 0.016).abs() < 1e-12),
            "all-owner native money is independent of the selected raw page: {stats}"
        );
        assert_eq!(stats["unavailable_sessions"], 0);
        let rows = stats["sessions"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            if page_number < 3 { 2 } else { 1 },
            "raw session projection must honor the bounded requested page"
        );
        for row in rows {
            seen.push(row["id"].as_str().unwrap().to_owned());
        }
        assert_eq!(stats["session_page"]["has_more"], page_number < 3);
        if page_number < 3 {
            after = stats["session_page"]["next_after"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(Some(&after), seen.last());
        } else {
            assert!(stats["session_page"]["next_after"].is_null());
        }
    }
    server.abort();
    assert_eq!(
        seen, ids,
        "stable keyset pages cover every retained owner once"
    );
    assert_eq!(
        history::accounting(&service, 0, "", 100).unwrap()["records"],
        frozen,
        "paging cannot mutate frozen paid money or accounting provenance"
    );
}

#[tokio::test]
async fn dashboard_population_money_does_not_require_decoding_off_page_raw_history() {
    let directory = tempfile::tempdir().unwrap();
    let ids = (0..3).map(|n| format!("{n:064x}")).collect::<Vec<_>>();
    let poll = |service: Arc<Service>| async move {
        let response = dashboard_stats(
            State(service),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            HeaderMap::new(),
            axum::extract::Query(SessionPage {
                after_id: String::new(),
                limit: Some(2),
            }),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    };
    let frozen;
    {
        let service = Arc::new(checkpoint_service(directory.path()));
        for (n, id) in ids.iter().enumerate() {
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(format!("off-page-raw-{id}")),
                request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
                ..Session::default()
            };
            let input = (0..if n == 2 { 12 } else { 1 })
                .map(|item| json!({"role":"user","content":format!("retained owner {n} source {item}")}))
                .collect::<Vec<_>>();
            session.ingest(&input).unwrap();
            numeric_primary(&mut session, 0);
            save(&service, id, &session).unwrap();
            let restored = history::restore(&service, id).unwrap();
            assert_eq!(restored.history.len(), input.len());
            assert_eq!(restored.primary.calls, 1);
            assert!((restored.primary.cost_usd - 0.002).abs() < 1e-12);
        }
        frozen = history::accounting(&service, 0, "", 100).unwrap()["records"].clone();
        assert_eq!(frozen.as_array().unwrap().len(), 3);
        let stats = poll(service).await;
        assert!(
            stats["cost_estimate"]["observed_primary_cost"]
                .as_f64()
                .is_some_and(|money| (money - 0.006).abs() < 1e-12)
        );
        assert_eq!(stats["unavailable_sessions"], 0);
    }
    // Reopen with a tighter raw-ancestry working set, not a changed retention
    // age or disk cap. The off-page owner's paid numeric record still fits.
    let mut bounded = checkpoint_service(directory.path());
    bounded.config.history_max_nodes = 2;
    let service = Arc::new(bounded);
    for id in &ids[..2] {
        let restored = history::restore(&service, id).unwrap();
        assert_eq!(restored.history.len(), 1);
        assert_eq!(restored.primary.calls, 1);
        assert!((restored.primary.cost_usd - 0.002).abs() < 1e-12);
    }
    assert!(
        history::restore(&service, &ids[2]).is_err(),
        "oversized raw authority is backpressure, not a new or expired owner"
    );
    assert_eq!(
        history::accounting(&service, 0, "", 100).unwrap()["records"],
        frozen
    );
    let stats = poll(service.clone()).await;
    let rows = stats["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], ids[0]);
    assert_eq!(rows[1]["id"], ids[1]);
    assert_eq!(stats["session_page"]["has_more"], true);
    assert_eq!(
        history::accounting(&service, 0, "", 100).unwrap()["records"],
        frozen,
        "dashboard reads cannot rewrite the frozen paid/provenance observations"
    );
    assert!(
        stats["cost_estimate"]["observed_primary_cost"]
            .as_f64()
            .is_some_and(|money| (money - 0.006).abs() < 1e-12),
        "population native money must not require off-page raw ancestry to fit the working set: {stats}"
    );
    assert_eq!(
        stats["unavailable_sessions"], 0,
        "off-page raw backpressure is not a failed detailed row on this page"
    );
}

#[test]
fn dashboard_all_attempt_lane_totals_survive_paging_restart_and_raw_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let start = 20_000_000;
    let ids = [format!("{:064x}", 911), format!("{:064x}", 912)];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let poll = |service: Arc<Service>| {
        runtime.block_on(async {
            let response = dashboard_stats(
                State(service),
                ConnectInfo("127.0.0.1:1".parse().unwrap()),
                HeaderMap::new(),
                axum::extract::Query(SessionPage {
                    after_id: String::new(),
                    limit: Some(1),
                }),
            )
            .await
            .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        })
    };
    let mut frozen = Value::Null;
    let mut stats = Vec::new();
    at_decision_clock(&[start], || {
        let service = checkpoint_service(directory.path());
        for (n, id) in ids.iter().enumerate() {
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(format!("all-attempt-dashboard-{id}")),
                request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
                ..Session::default()
            };
            session
                .ingest(&[json!({"role":"user","content":"private retained source"})])
                .unwrap();
            let usage = json!({"input_tokens":1000,"output_tokens":0,
                "input_tokens_details":{"cached_tokens":0}});
            // Numeric receipt-owner seam: one known and one unavailable
            // reviewer contribution; neither is inferred from primary coverage.
            session.shadow.observe_with_rates(
                "gpt-6.1-sol",
                &usage,
                true,
                if n == 0 {
                    receipt_rates("gpt-6.1-sol", &usage, None, true)
                } else {
                    None
                },
            );
            for turn in 0..if n == 0 { 2 } else { 1 } {
                numeric_primary(&mut session, turn);
                save(&service, id, &session).unwrap();
            }
            let restored = history::restore(&service, id).unwrap();
            assert_eq!(restored.primary.calls, if n == 0 { 2 } else { 1 });
            assert_eq!(restored.shadow.calls, 1);
            assert_eq!(restored.shadow.unavailable_cost_calls, n as u64);
            assert!((restored.shadow.cost_usd - if n == 0 { 0.002 } else { 0.0 }).abs() < 1e-12);
        }
        frozen = history::accounting(&service, 0, "", 100).unwrap()["records"].clone();
        assert_eq!(frozen.as_array().unwrap().len(), 3);
        drop(service);
        let live = poll(Arc::new(checkpoint_service(directory.path())));
        assert_eq!(live["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(live["session_page"]["has_more"], true);
        stats.push(live);
    });
    at_decision_clock(&[start + 120 * 86400], || {
        let service = Arc::new(checkpoint_service(directory.path()));
        let expired = poll(service.clone());
        assert!(expired["sessions"].as_array().unwrap().is_empty());
        assert_eq!(
            history::accounting(&service, 0, "", 100).unwrap()["records"],
            frozen
        );
        stats.push(expired);
    });
    // Qualify both phases and frozen history before the new projection grader.
    for (phase, report) in ["live bounded page", "expired raw after restart"]
        .iter()
        .zip(stats)
    {
        assert_eq!(report["usage"]["primary"]["calls"], 3, "{phase}: {report}");
        assert_eq!(report["usage"]["primary"]["input_tokens"], 3000);
        assert_eq!(report["usage"]["primary"]["unavailable_cost_calls"], 0);
        assert!((report["usage"]["primary"]["cost_usd"].as_f64().unwrap() - 0.006).abs() < 1e-12);
        assert_eq!(report["usage"]["reviewer"]["calls"], 2);
        assert_eq!(report["usage"]["reviewer"]["input_tokens"], 2000);
        assert_eq!(report["usage"]["reviewer"]["unavailable_cost_calls"], 1);
        assert!((report["usage"]["reviewer"]["cost_usd"].as_f64().unwrap() - 0.002).abs() < 1e-12);
        assert_eq!(
            report["usage"]["unavailable_owners"], 0,
            "readable independent paid totals must not inherit raw expiry: {phase}: {report}"
        );
    }
}

#[tokio::test]
async fn dashboard_failure_only_describes_the_latest_failed_primary() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let body = json!({"model":"gpt-6-luna", "input":[]});
    for (n, outcome, failure_request, visible) in [
        (0, "completed", Some("old"), false),
        (1, "failed", Some("old"), false),
        (2, "failed", Some("latest"), true),
        (3, "dispatched", Some("old"), false),
        (4, "failed", None, false),
    ] {
        let mut session = Session {
            failed_primaries: 3,
            last_failure: Some(json!({"reason":"old failure"})),
            last_primary_dispatch: Some(dispatch_measurement("latest", "primary", &body)),
            ..Session::default()
        };
        session.last_primary_dispatch.as_mut().unwrap().outcome = outcome.into();
        if let Some(request_id) = failure_request {
            session.last_failure.as_mut().unwrap()["request_id"] = json!(request_id);
        }
        let id = format!("{n:064x}");
        save(&service, &id, &session).unwrap();
        let response = dashboard_stats(
            State(service.clone()),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            HeaderMap::new(),
            axum::extract::Query(SessionPage::default()),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let stats: Value = serde_json::from_slice(&bytes).unwrap();
        let projected = stats["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["id"] == id)
            .unwrap();
        assert_eq!(
            projected["last_failure"].is_object(),
            visible,
            "{outcome}, {failure_request:?}"
        );
        assert_eq!(projected["failed_primaries"], 3);
        assert_eq!(
            load_existing(&service, &id).unwrap().last_failure,
            session.last_failure
        );
    }
}

#[test]
fn dashboard_paid_fallback_settles_only_the_matching_primary_failure() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let body = json!({"model":"gpt-6-luna", "input":[]});
    let retained = Session {
        last_failure: Some(json!({"request_id":"old", "reason":"old failure"})),
        last_primary_dispatch: Some(dispatch_measurement("latest", "primary", &body)),
        failed_primaries: 2,
        ..Session::default()
    };
    for (request, completed, copied) in [
        ("latest", false, true),
        ("latest", true, true),
        ("unrelated", false, false),
    ] {
        save(&service, "failure-fallback", &retained).unwrap();
        let mut paid = retained.clone();
        finish_measurement(&mut paid.last_primary_dispatch, None, completed);
        paid.last_failure =
            (!completed).then(|| json!({"request_id":"latest", "reason":"new failure"}));
        save_paid_accounting(&service, "failure-fallback", &paid, request).unwrap();
        let actual = load_existing(&service, "failure-fallback").unwrap();
        assert_eq!(
            actual.last_failure,
            if copied {
                paid.last_failure
            } else {
                retained.last_failure.clone()
            }
        );
        assert_eq!(actual.failed_primaries, 2);
    }
}

#[test]
fn dashboard_counts_reuse_equivalent_prompts_without_repricing_receipts() {
    let base = json!({"model":"gpt-6-luna", "input":[
        {"role":"user","content":"large owned source 🦀 ".repeat(1000)},
        {"type":"function_call_output","call_id":"opaque","output":"{\"text\":\"literal\"}"}
    ], "instructions":"fixed instruction", "tools":[{"type":"function","name":"fixture","parameters":{"type":"object"}}],
    "text":{"format":{"type":"json_object"}}});
    let mut transport = base.clone();
    transport["model"] = json!("other-model");
    transport["max_output_tokens"] = json!(900);
    transport["prompt_cache_key"] = json!("other-affinity");
    transport["stream"] = json!(true);
    let mut changed = base.clone();
    changed["instructions"] = json!("genuinely different instruction");
    let adapted = codex_body(&base);
    let invalid = json!({"input":"not an array"});
    let bodies = [&base, &transport, &changed, &adapted, &invalid];
    let mut counts = PromptTokenCounts::default();
    for body in bodies {
        assert_eq!(
            counts.count(body),
            crate::savings::prompt_projection(body).map(|p| p.2)
        );
    }
    let mut equivalent = PromptTokenCounts::default();
    let before = equivalent.count(&base);
    assert_eq!(equivalent.count(&transport), before);
    assert_eq!(
        equivalent.0.len(),
        1,
        "the equivalent view executes no second tokenizer pass"
    );
    assert_eq!(
        equivalent.count(&changed),
        crate::savings::prompt_input_tokens(&changed)
    );
    assert_eq!(equivalent.0.len(), 2);
    crate::prompt_quantity::reset_token_work();
    let dispatched = dispatch_measurement("request", "primary", &base);
    assert_eq!(
        dispatched.outgoing_input_tokens, None,
        "constructing telemetry must not eagerly count a prompt"
    );
    assert_eq!(crate::prompt_quantity::token_work(), (0, 0));
    assert_eq!(
        dispatched.receipt_input_tokens, None,
        "an estimate is never a provider receipt"
    );
}

#[tokio::test]
async fn dashboard_read_view_preserves_authority_and_reloads_each_poll() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let mut ids = Vec::new();
    for n in 0..7 {
        let id = format!("{n:064x}");
        let mut session = Session {
            history_shared: true,
            history_namespace: Some("owned-dashboard".into()),
            ..Session::default()
        };
        observe_source(&mut session, &format!("shared retained source {n}"));
        save(&service, &id, &session).unwrap();
        ids.push(id);
    }
    let index_path = directory.path().join("history.index");
    let frozen = std::fs::read(&index_path).unwrap();
    let view = history::ReadView::new(&service).unwrap();
    for id in &ids {
        let restored = view.restore(id).unwrap();
        let expected = load(&service, id).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let input = restored
            .history
            .iter()
            .map(|i| i.value.clone())
            .collect::<Vec<_>>();
        assert!(view.advice("owned-dashboard", &input).is_none());
    }
    let poll = || {
        dashboard_stats(
            State(service.clone()),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            HeaderMap::new(),
            axum::extract::Query(SessionPage::default()),
        )
    };
    let response = poll().await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let stats: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stats["sessions"].as_array().unwrap().len(), 7);
    assert_eq!(stats["unavailable_sessions"], 0);
    assert_eq!(
        std::fs::read(&index_path).unwrap(),
        frozen,
        "a poll cannot rewrite authority"
    );

    // A new poll must not reuse a previously valid view or revive shared history
    // from its diagnostic checkpoint when the authoritative index is broken.
    std::fs::write(&index_path, b"invalid authoritative index").unwrap();
    let response = poll().await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let stats: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(stats["sessions"].as_array().unwrap().is_empty());
    assert_eq!(stats["unavailable_sessions"], 7);
    assert_eq!(
        std::fs::read(&index_path).unwrap(),
        b"invalid authoritative index"
    );
}

#[test]
fn dashboard_read_view_rechecks_expiry_pins_and_capacity_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    let ids = ["a".repeat(64), "b".repeat(64)];
    for (n, id) in ids.iter().enumerate() {
        let mut session = Session {
            history_shared: true,
            history_namespace: Some("view-retention".into()),
            ..Session::default()
        };
        observe_source(&mut session, &format!("retained owner {n}"));
        save(&service, id, &session).unwrap();
    }
    let path = directory.path().join("history.index");
    let original = history::fixture::read_index(&path);
    let old_view = history::ReadView::new(&service).unwrap();
    let mut expired: Value = serde_json::from_slice(&original).unwrap();
    for reference in expired["tips"].as_object_mut().unwrap().values_mut() {
        reference["at"] = json!(0);
    }
    let staged = serde_json::to_vec(&expired).unwrap();
    history::fixture::write_index(&path, &staged);
    let pin = history::pin(&service, &ids[0]).unwrap();
    let pinned = history::ReadView::new(&service).unwrap();
    assert!(pinned.shared(&ids[0]).unwrap().is_some());
    assert!(pinned.shared(&ids[1]).is_err());
    drop(pin);
    let released = history::ReadView::new(&service).unwrap();
    assert!(released.shared(&ids[0]).is_err());
    assert!(
        old_view.shared(&ids[1]).unwrap().is_some(),
        "an existing view is one coherent generation"
    );
    assert_eq!(history::fixture::read_index(&path), staged);

    history::fixture::write_index(&path, &original);
    service.config.history_max_nodes = 1;
    let pin = history::pin(&service, &ids[0]).unwrap();
    let bounded = history::ReadView::new(&service).unwrap();
    assert!(bounded.shared(&ids[0]).unwrap().is_some());
    assert!(
        bounded.shared(&ids[1]).unwrap().is_some(),
        "a working-set cap cannot expire an unrelated retained owner"
    );
    assert_eq!(history::fixture::read_index(&path), original);
    drop(pin);
}

#[test]
fn dashboard_paid_fallback_fences_terminal_measurements_by_request_and_lane() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let body = json!({"model":"gpt-6-luna","input":[{"role":"user","content":"retained source"}]});
    let mut retained = Session::default();
    observe_source(&mut retained, "retained source");
    retained.last_primary_dispatch = Some(dispatch_measurement("newer-primary", "primary", &body));
    retained.last_reviewer_dispatch =
        Some(dispatch_measurement("review-parent", "reviewer", &body));
    let mut paid = retained.clone();
    paid.shadow.calls = 1;
    let record = paid.last_reviewer_dispatch.as_mut().unwrap();
    record.request_id = "review-attempt".into();
    record.parent_request_id = Some("review-parent".into());
    finish_measurement(
        &mut paid.last_reviewer_dispatch,
        Some(&json!({"usage":{"input_tokens":100,"output_tokens":1}})),
        true,
    );
    paid.last_primary_dispatch = Some(dispatch_measurement("unrelated-primary", "primary", &body));
    finish_measurement(&mut paid.last_primary_dispatch, None, false);
    paid.title = Some("failed-publication-material".into());
    for (request, lane, copied) in [
        ("review-parent", "reviewer", true),
        ("other-parent", "reviewer", false),
        ("review-parent", "primary", false),
    ] {
        save(&service, "fallback", &retained).unwrap();
        paid.last_reviewer_dispatch.as_mut().unwrap().lane = lane.into();
        save_paid_accounting(&service, "fallback", &paid, request).unwrap();
        let actual = load_existing(&service, "fallback").unwrap();
        assert_eq!(actual.shadow.calls, 1);
        assert_eq!(actual.title, retained.title);
        assert_eq!(
            actual.last_primary_dispatch.unwrap().request_id,
            "newer-primary"
        );
        let reviewer = actual.last_reviewer_dispatch.unwrap();
        assert_eq!(
            reviewer.outcome,
            if copied { "completed" } else { "dispatched" }
        );
        assert_eq!(reviewer.receipt_input_tokens, copied.then_some(100));
    }
}

#[tokio::test]
async fn dashboard_transcript_uses_real_private_peer_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.auth_token = Some("fixture-dashboard-token".into());
    let service = Arc::new(service);
    let id = "a".repeat(64);
    let mut session = Session::default();
    observe_source(&mut session, "owned private source");
    save(&service, &id, &session).unwrap();
    for peer in ["192.0.2.1:1234", "[2001:db8::1]:1234"] {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
        headers.insert("host", "localhost:8787".parse().unwrap());
        let failure = dashboard_transcript(
            State(service.clone()),
            ConnectInfo(peer.parse().unwrap()),
            headers,
            axum::extract::Path(id.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.0, StatusCode::UNAUTHORIZED);
    }
    let response = dashboard_transcript(
        State(service),
        ConnectInfo("127.0.0.1:1".parse().unwrap()),
        HeaderMap::new(),
        axum::extract::Path(id),
    )
    .await
    .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let transcript: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        transcript["canonical_input"][0]["content"],
        "owned private source"
    );
}

#[test]
fn dashboard_measurement_retains_provider_output_without_inventing_missing_usage() {
    let body = json!({"input":[]});
    let mut record = Some(dispatch_measurement("receipt", "primary", &body));
    finish_measurement(
        &mut record,
        Some(&json!({"usage":{"input_tokens":120,"output_tokens":37}})),
        true,
    );
    let encoded = serde_json::to_value(&record).unwrap();
    assert_eq!(encoded["receipt_output_tokens"], 37);
    finish_measurement(
        &mut record,
        Some(&json!({"usage":{"input_tokens":120}})),
        false,
    );
    assert!(serde_json::to_value(&record).unwrap()["receipt_output_tokens"].is_null());
    // Older saved numeric measurements remain readable with output unknown.
    let mut old = encoded;
    old.as_object_mut().unwrap().remove("receipt_output_tokens");
    let decoded: crate::proxy_state::DispatchMeasurement = serde_json::from_value(old).unwrap();
    assert!(serde_json::to_value(decoded).unwrap()["receipt_output_tokens"].is_null());
}

#[tokio::test]
async fn dashboard_context_states_partition_sources_not_memories() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let mut session = Session {
        history_namespace: Some("context-partition".into()),
        ..Session::default()
    };
    for n in 0..5 {
        observe_source(&mut session, &format!("source {n}"));
    }
    session.apply_advice(&session.groups(), &json!({"removable":[1,2],"protected":[3],"memories":[{"source_ids":[1],"text":"Remember source one"}]})).unwrap();
    session.last_submitted_ids = vec![2, 3, 4, 5];
    history::publish(&service, &session, &session).unwrap();
    save(&service, &"c".repeat(64), &session).unwrap();
    let response = dashboard_stats(
        State(service),
        ConnectInfo("127.0.0.1:1".parse().unwrap()),
        HeaderMap::new(),
        axum::extract::Query(SessionPage::default()),
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let stats: Value = serde_json::from_slice(&bytes).unwrap();
    let c = &stats["sessions"][0]["context"];
    assert_eq!(c["history_items"], 5);
    assert_eq!(c["removed_items"], 1);
    assert_eq!(
        c["removable_items"], 1,
        "omitted authorized sources are not double counted as removable"
    );
    assert_eq!(c["protected_items"], 2);
    assert_eq!(c["unreviewed_items"], 1);
    assert_eq!(
        c["memories"], 1,
        "memory facts are separate from source states"
    );
}
