// Isolated native component acceptance, not real provider or browser traffic.
#[cfg(target_os = "linux")]
#[test]
fn retention_many_owner_process_resources_stay_bounded() {
    use std::io::Write as _;
    let directory = tempfile::tempdir().unwrap();
    let temporary = directory.path().join("temporary");
    std::fs::create_dir(&temporary).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "proxy::tests::retention_many_owner_resource_child",
            "--nocapture",
        ])
        .env("CARRY_TEST_RESOURCE_ROOT", directory.path())
        .env("TMPDIR", &temporary)
        .env("SQLITE_TMPDIR", &temporary)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(240);
    let mut sampled_disk_peak = 0;
    let mut sampled_temp_peak = 0;
    let mut timed_out = false;
    while child.try_wait().unwrap().is_none() {
        sampled_disk_peak = sampled_disk_peak.max(resource_fixture_disk_bytes(directory.path()));
        // SQLite can unlink temporary files while they remain open. Include
        // those descriptors; directory entries alone cannot prove a temp bound.
        let unlinked = std::fs::read_dir(format!("/proc/{}/fd", child.id()))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                std::fs::read_link(entry.path()).is_ok_and(|target| {
                    target.starts_with(&temporary)
                        && target.to_string_lossy().ends_with(" (deleted)")
                })
            })
            .filter_map(|entry| std::fs::metadata(entry.path()).ok())
            .map(|metadata| metadata.len())
            .sum::<u64>();
        sampled_temp_peak =
            sampled_temp_peak.max(resource_fixture_disk_bytes(&temporary).saturating_add(unlinked));
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            child.kill().unwrap();
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let records = stdout
        .lines()
        .filter_map(|line| {
            line.split_once("retention-resource-child:")
                .map(|(_, record)| record)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        1,
        "missing/duplicate resource evidence: {stdout}"
    );
    let mut report: Value = serde_json::from_str(records[0]).unwrap();
    report["sampled_disk_peak_bytes"] = json!(sampled_disk_peak);
    report["sampled_temp_peak_bytes"] = json!(sampled_temp_peak);
    report["sample_period_ms"] = json!(25);
    // Direct Write bypasses libtest's successful-output capture. Hosted CI
    // retains actual measurements even without changing the workflow flags.
    writeln!(std::io::stdout(), "retention-resource-acceptance:{report}").unwrap();
    // Retain sampled evidence even when a later child resource grader fails.
    assert!(
        !timed_out && output.status.success(),
        "isolated resource fixture failed: timeout={timed_out}, {output:?}"
    );
    assert!(sampled_disk_peak <= 512 * 1024 * 1024, "{report}");
    assert!(sampled_temp_peak <= 8 * 1024 * 1024, "{report}");
}

#[cfg(target_os = "linux")]
fn resource_fixture_disk_bytes(directory: &Path) -> u64 {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            match entry.metadata() {
                Ok(metadata) if metadata.is_dir() => resource_fixture_disk_bytes(&entry.path()),
                Ok(metadata) => metadata.len(),
                // Atomic publication/maintenance can remove a file between
                // directory enumeration and metadata sampling.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => panic!("resource disk sampling failed: {error}"),
            }
        })
        .sum()
}

#[cfg(target_os = "linux")]
fn resource_fixture_memory() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            * 1024
    };
    (field("VmRSS:"), field("VmHWM:"))
}

#[cfg(target_os = "linux")]
#[test]
fn retention_many_owner_resource_child() {
    use std::io::Write as _;
    let Ok(root) = std::env::var("CARRY_TEST_RESOURCE_ROOT") else {
        return;
    };
    let state = Path::new(&root).join("state");
    std::fs::create_dir(&state).unwrap();
    let reopen = || {
        let mut configured = checkpoint_service(&state);
        configured.config.history_max_bytes = 256 * 1024;
        configured.config.history_max_references = 16;
        configured.config.history_disk_bytes = 512 * 1024 * 1024;
        Arc::new(configured)
    };
    let service = reopen();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let start = 20_000_000;
    let owners = 384;
    let content_bytes = 192 * 1024;
    let mut warm_rss = 0;
    let mut max_save_us = 0;
    let mut maintenance_us = Vec::new();
    let mut report_us = Vec::new();
    let mut page_sizes = Vec::new();
    let elapsed = std::time::Instant::now();
    at_decision_clock(&[start], || {
        for n in 0..owners {
            let id = format!("{n:064x}");
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(format!("resource-owner-{n}")),
                request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
                ..Session::default()
            };
            session
                .ingest(&[
                    json!({"role":"user","content":format!("{n}:{}", "x".repeat(content_bytes))}),
                ])
                .unwrap();
            numeric_primary(&mut session, 0);
            let began = std::time::Instant::now();
            save(&service, &id, &session).unwrap();
            max_save_us = max_save_us.max(began.elapsed().as_micros());
            if n == 31 {
                warm_rss = resource_fixture_memory().0;
            }
        }
        for n in [0, 31, owners - 1] {
            let restored = history::restore(&service, &format!("{n:064x}")).unwrap();
            assert_eq!(restored.history.len(), 1);
            assert!(
                restored.render_primary()[0]["content"]
                    .as_str()
                    .unwrap()
                    .len()
                    >= content_bytes
            );
            assert_eq!(restored.primary.calls, 1);
            assert!((restored.primary.cost_usd - 0.002).abs() < 1e-12);
        }
    });
    // Logical expiry must leave numeric records readable without decoding the
    // large raw population. Reopen independently for each reporting path.
    at_decision_clock(&[start + 60 * 86400], || {
        for query_window in [false, true] {
            let reopened = reopen();
            let began = std::time::Instant::now();
            let report = if query_window {
                history::accounting_window(&reopened, start - 1, start + 30 * 86400).unwrap()
            } else {
                runtime.block_on(async {
                    let response = dashboard_stats(
                        State(reopened),
                        ConnectInfo("127.0.0.1:1".parse().unwrap()),
                        HeaderMap::new(),
                        axum::extract::Query(SessionPage::default()),
                    )
                    .await
                    .unwrap();
                    let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
                        .await
                        .unwrap();
                    page_sizes.push(bytes.len());
                    serde_json::from_slice::<Value>(&bytes).unwrap()
                })
            };
            report_us.push(began.elapsed().as_micros());
            if query_window {
                // Every owner lacks a pre-window opening; no invented zero.
                assert_eq!(report["coverage"]["missing_opening_owners"], owners);
                assert_eq!(report["coverage"]["included_owners"], 0);
            } else {
                assert_eq!(report["usage"]["primary"]["calls"], owners);
                assert!(
                    (report["usage"]["primary"]["cost_usd"].as_f64().unwrap()
                        - 0.002 * owners as f64)
                        .abs()
                        < 1e-12
                );
            }
        }
        // A native maintenance publication expires bounded batches; do not
        // substitute a mass SQL DELETE or call this full physical reclamation.
        for n in 0..8 {
            let id = format!("{:064x}", owners + n);
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(format!("resource-maintenance-{n}")),
                ..Session::default()
            };
            session
                .ingest(&[json!({"role":"user","content":"maintenance"})])
                .unwrap();
            let began = std::time::Instant::now();
            save(&service, &id, &session).unwrap();
            maintenance_us.push(began.elapsed().as_micros());
        }
        assert!(history::restore(&checkpoint_service(&state), &format!("{:064x}", 0)).is_err());
    });
    let (final_rss, peak_rss) = resource_fixture_memory();
    let db = rusqlite::Connection::open_with_flags(
        state.join("history.index"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (accounting, snapshots): (u64, u64) = db
        .query_row(
            "SELECT sum(kind='accounting'),sum(kind='snapshots') FROM records",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(accounting, owners);
    assert!(
        snapshots < owners,
        "maintenance did not expire any raw owners"
    );
    let report = json!({
        "scope":"isolated debug native save/restore/report/expiry component; no provider calls; sampled disk/temp peaks",
        "owners":owners,"source_bytes_per_owner":content_bytes,
        "working_set_bytes":256*1024,"disk_admission_bytes":512*1024*1024,
        "warm_after_owners":32,"warm_rss_bytes":warm_rss,
        "final_rss_bytes":final_rss,"kernel_peak_rss_bytes":peak_rss,
        "max_save_us":max_save_us,"report_us":report_us,
        "maintenance_publication_us":maintenance_us,"response_bytes":page_sizes,
        "retained_accounting":accounting,"remaining_snapshots":snapshots,
        "elapsed_ms":elapsed.elapsed().as_millis(),
    });
    writeln!(std::io::stdout(), "retention-resource-child:{report}").unwrap();
    assert!(peak_rss <= 128 * 1024 * 1024, "{report}");
    assert!(
        peak_rss.saturating_sub(warm_rss) <= 32 * 1024 * 1024,
        "{report}"
    );
    assert!(max_save_us <= 5_000_000, "{report}");
    assert!(
        maintenance_us.iter().all(|micros| *micros <= 5_000_000),
        "{report}"
    );
    assert!(
        report_us.iter().all(|micros| *micros <= 5_000_000),
        "{report}"
    );
}

#[tokio::test]
async fn dashboard_unfit_envelope_and_individual_row_return_507_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    let id = format!("{:064x}", 935);
    let mut session = Session {
        history_shared: true,
        history_namespace: Some("unfit-dashboard-owner".into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Session::default()
    };
    session
        .ingest(&[json!({"role":"user","content":"retained unfit row"})])
        .unwrap();
    numeric_primary(&mut session, 0);
    save(&service, &id, &session).unwrap();
    let frozen = std::fs::read(directory.path().join("history.index")).unwrap();
    let mirror = std::fs::read(directory.path().join(format!("{id}.json"))).unwrap();
    let response = dashboard_stats(
        State(Arc::new(checkpoint_service(directory.path()))),
        ConnectInfo("127.0.0.1:1".parse().unwrap()),
        HeaderMap::new(),
        axum::extract::Query(SessionPage::default()),
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let report: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(report["usage"]["primary"]["calls"], 1);
    assert!((report["usage"]["primary"]["cost_usd"].as_f64().unwrap() - 0.002).abs() < 1e-12);
    let row_limit = u32::try_from(bytes.len() - 1).unwrap();
    assert!(
        mirror.len() < row_limit as usize,
        "row fixture must still fit native restoration"
    );
    for bound in [row_limit, 256] {
        service.config.history_max_bytes = bound;
        let shared = Arc::new(service);
        if bound == row_limit {
            assert_eq!(history::restore(&shared, &id).unwrap().primary.calls, 1);
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/carry/dashboard/stats", get(dashboard_stats))
            .with_state(shared);
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let response = reqwest::Client::new()
            .get(format!("http://{address}/carry/dashboard/stats?limit=1"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::INSUFFICIENT_STORAGE,
            "bound {bound}"
        );
        let error: Value = response.json().await.unwrap();
        assert!(error["sessions"].is_null());
        assert_eq!(
            error["error"]["message"],
            if bound == row_limit {
                "dashboard row exceeds working-set bound"
            } else {
                "dashboard page exceeds working-set bound"
            }
        );
        server.abort();
        assert_eq!(
            std::fs::read(directory.path().join("history.index")).unwrap(),
            frozen
        );
        assert_eq!(
            std::fs::read(directory.path().join(format!("{id}.json"))).unwrap(),
            mirror
        );
        service = checkpoint_service(directory.path());
    }
}
