// Actual handler first, then bundled-SQLite/source-bound phase diagnostics.
// This is a synthetic indexed fork cohort, never private history/provider traffic.
fn detail_resources() -> (u64, u64, u64) {
    let io = std::fs::read_to_string("/proc/self/io").unwrap();
    let field = |name: &str| {
        io.lines()
            .find_map(|s| s.strip_prefix(name))
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the supplied live, correctly aligned rusage.
    let cpu = unsafe {
        assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
        let r = usage.assume_init();
        ((r.ru_utime.tv_sec + r.ru_stime.tv_sec) * 1_000_000
            + r.ru_utime.tv_usec
            + r.ru_stime.tv_usec) as u64
    };
    (field("rchar:"), field("read_bytes:"), cpu)
}
fn detail_phase<T>(name: &str, view: &history::ReadView, f: impl FnOnce() -> T) -> T {
    let before = detail_resources();
    let sql = view.read_profile();
    let began = std::time::Instant::now();
    let result = f();
    let after = detail_resources();
    let end = view.read_profile();
    let calls = end
        .1
        .iter()
        .map(|(k, v)| (k, v - sql.1.get(k).unwrap_or(&0)))
        .collect::<std::collections::BTreeMap<_, _>>();
    println!(
        "detail-phase:{}",
        json!({"phase":name,"elapsed_us":began.elapsed().as_micros(),"cpu_us":after.2-before.2,"logical_bytes":after.0-before.0,"physical_bytes":after.1-before.1,"sqlite_cache_misses":end.0-sql.0,"get_calls":calls})
    );
    result
}
fn detail_poll(
    runtime: &tokio::runtime::Runtime,
    service: Arc<Service>,
    page: SessionPage,
) -> Value {
    let response = runtime
        .block_on(dashboard_stats(
            State(service),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            HeaderMap::new(),
            axum::extract::Query(page),
        ))
        .unwrap();
    let bytes = runtime
        .block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
#[test]
fn dashboard_indexed_detail_reads_reuse_bounded_sqlite_pages() {
    // rchar and RUSAGE_SELF belong to a process, not a libtest thread.
    // Keep the ordinary parallel registration, but measure one exact child.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let noise = (std::env::var("CARRY_TEST_DETAIL_PARENT_IO").as_deref() == Ok("1")).then(|| {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0; 65536]).unwrap();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut bytes = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                bytes += std::fs::read(file.path()).unwrap().len() as u64;
                std::thread::sleep(Duration::from_millis(1));
            }
            bytes
        })
    });
    let output = tempfile::tempfile().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "proxy::tests::dashboard_indexed_detail_reads_child",
            "--nocapture",
        ])
        .env("CARRY_TEST_DETAIL_CHILD", "1")
        .stdout(output.try_clone().unwrap())
        .stderr(output.try_clone().unwrap())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut timed_out = false;
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            child.kill().unwrap();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(noise) = noise {
        let bytes = noise.join().unwrap();
        assert!(
            bytes > 2 * 4096,
            "qualify concurrent unrelated parent reads"
        );
        println!(
            "detail-parent-io:{}",
            json!({"bytes":bytes,"parent_pid":std::process::id(),"child_pid":child.id()})
        );
    }
    use std::io::{Read as _, Seek as _, Write as _};
    let mut output = output;
    output.rewind().unwrap();
    let mut raw = Vec::new();
    output.read_to_end(&mut raw).unwrap();
    std::io::stdout().write_all(&raw).unwrap();
    assert!(
        !timed_out && status.success(),
        "detail child: {status}, timed_out={timed_out}"
    );
}

#[test]
fn dashboard_indexed_detail_reads_child() {
    if std::env::var("CARRY_TEST_DETAIL_CHILD").as_deref() != Ok("1") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let reopen = || Arc::new(checkpoint_service(directory.path()));
    let service = reopen();
    let namespace = "detail-amplification";
    let parent = "f".repeat(64);
    let common = (0..384)
        .map(|n| json!({"role":"user","content":format!("common {n} {}", "c".repeat(2048))}))
        .collect::<Vec<_>>();
    let mut subject = Session {
        history_shared: true,
        history_namespace: Some(namespace.into()),
        request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
        ..Default::default()
    };
    subject.ingest(&common).unwrap();
    for item in &mut subject.history {
        item.exposed = true;
    }
    subject.last_submitted_ids = subject.history.iter().map(|i| i.id).collect();
    subject.request_evidence.as_mut().unwrap().bind_archive(
        &history::FilePolicy::new(&service.config),
        &parent,
        Some(namespace),
    );
    for event in 0..32 {
        numeric_primary(&mut subject, event);
    }
    save(&service, &parent, &subject).unwrap();
    subject = load(&service, &parent).unwrap();
    let cut = subject
        .request_evidence
        .as_ref()
        .unwrap()
        .comparison_context();
    let mut reviewed = subject.clone();
    reviewed.completed_review_len = reviewed.history.len();
    let groups = reviewed.groups();
    let member = groups.iter().find(|g| !g.pinned).unwrap().members[0];
    reviewed
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[member],"memories":[]}),
        )
        .unwrap();
    history::publish(&service, &subject, &reviewed).unwrap();
    let mut ids = Vec::new();
    let mut probe_input = Vec::new();
    for n in 0..12 {
        let id = format!("{n:064x}");
        let mut session = Session {
            history_shared: true,
            history_namespace: Some(namespace.into()),
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Default::default()
        };
        let mut input = common.clone();
        input.extend((0..128).map(
            |j| json!({"role":"user","content":format!("owner {n} item {j} {}", "x".repeat(2048))}),
        ));
        session.ingest(&input).unwrap();
        for item in &mut session.history {
            item.exposed = true;
        }
        session.last_submitted_ids = session.history.iter().map(|i| i.id).collect();
        let evidence = session.request_evidence.as_mut().unwrap();
        evidence.bind_archive(
            &history::FilePolicy::new(&service.config),
            &id,
            Some(namespace),
        );
        evidence.inherit_comparison_context(&parent, Some(cut.clone()));
        for event in 0..32 {
            numeric_primary(&mut session, event);
        }
        save(&service, &id, &session).unwrap();
        if n == 0 {
            probe_input = input;
        }
        ids.push(id);
    }
    let frozen = std::fs::read(directory.path().join("history.index")).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut expected: Option<Value> = None;
    for (poll, limit) in [None, None, Some(1), None].into_iter().enumerate() {
        let before = detail_resources();
        let began = std::time::Instant::now();
        let value = detail_poll(
            &runtime,
            reopen(),
            SessionPage {
                after_id: String::new(),
                limit,
            },
        );
        let after = detail_resources();
        assert_eq!(
            value["sessions"].as_array().unwrap().len(),
            if limit == Some(1) { 1 } else { 13 }
        );
        assert_eq!(value["usage"]["primary"]["calls"], 416);
        assert!((value["usage"]["primary"]["cost_usd"].as_f64().unwrap() - 0.832).abs() < 1e-12);
        assert_eq!(value["unavailable_sessions"], 0);
        if let Some(e) = &expected {
            assert_eq!(value["usage"], e["usage"]);
            assert_eq!(value["cost_estimate"], e["cost_estimate"]);
            if limit.is_none() {
                assert_eq!(&value, e);
            } else {
                assert_eq!(value["sessions"][0], e["sessions"][0]);
            }
        } else {
            expected = Some(value.clone());
        }
        println!(
            "detail-handler:{}",
            json!({"poll":poll,"limit":limit,"elapsed_us":began.elapsed().as_micros(),"cpu_us":after.2-before.2,"logical_bytes":after.0-before.0,"physical_bytes":after.1-before.1,"response_bytes":serde_json::to_vec(&value).unwrap().len()})
        );
    }
    let expected = expected.unwrap();
    // Full keyset traversal from independently opened services, not a reduced
    // default/page limit workaround. Every row and all global money stay exact.
    let mut rows = Vec::new();
    let mut after_id = String::new();
    loop {
        let value = detail_poll(
            &runtime,
            reopen(),
            SessionPage {
                after_id,
                limit: Some(1),
            },
        );
        assert_eq!(value["usage"], expected["usage"]);
        assert_eq!(value["cost_estimate"], expected["cost_estimate"]);
        let page = value["sessions"].as_array().unwrap();
        assert_eq!(page.len(), 1);
        rows.extend(page.iter().cloned());
        if value["session_page"]["has_more"] == false {
            break;
        }
        after_id = value["session_page"]["next_after"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(rows.len() < 13);
    }
    assert_eq!(rows, expected["sessions"].as_array().unwrap().clone());
    let mut comparable = expected.clone();
    for row in comparable["sessions"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("updated_at");
    }
    println!(
        "detail-content-sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&comparable).unwrap())
    );
    let before = detail_resources();
    let began = std::time::Instant::now();
    let authority = history::ReadView::new(&service);
    let after = detail_resources();
    println!(
        "detail-open:{}",
        json!({"elapsed_us":began.elapsed().as_micros(),"cpu_us":after.2-before.2,"logical_bytes":after.0-before.0,"physical_bytes":after.1-before.1})
    );
    let view = authority.as_ref().unwrap();
    println!("detail-sqlite-policy:{}", view.sqlite_policy());
    detail_phase("frozen", view, || view.expired_accounting_totals().unwrap());
    for id in &ids {
        let numeric = detail_phase("current_load", view, || {
            load_dashboard_accounting(&service, id, &authority).unwrap()
        });
        detail_phase("current_estimate", view, || {
            dashboard_session_estimate(&numeric)
        });
        let mut detail = detail_phase("shared_restore", view, || {
            recover_success_prefix(&service, id, view.shared(id).unwrap().unwrap()).unwrap()
        });
        assert_eq!(detail.history.len(), 512);
        let estimate = detail_phase("detail_estimate", view, || {
            dashboard_session_estimate(&detail)
        });
        assert_eq!(
            estimate.coverage.primary_responses, 32,
            "ancestor context cannot become owned paid calls"
        );
        assert!((detail.primary.cost_usd - 0.064).abs() < 1e-12);
        let input = detail
            .history
            .iter()
            .map(|i| i.value.clone())
            .collect::<Vec<_>>();
        detail_phase("advice", view, || {
            view.advice(namespace, &input).unwrap().apply(&mut detail)
        });
        assert_eq!(
            detail.opinions.get(&member).map(String::as_str),
            Some("drop")
        );
        detail_phase("render", view, || {
            let groups = detail.groups();
            let removable = groups.iter().filter(|g| detail.drop_authorized(g)).count();
            assert!(removable > 0);
            serde_json::to_vec(&detail.render_primary()).unwrap()
        });
    }
    // Exercise the real shared advice owner, including absent suffix cuts.
    // Its fixed review-key range fits the existing 2 MiB page cache. After one
    // pass it must not reread unrelated BLOBs at every B-tree comparison.
    let first = serde_json::to_value(view.advice(namespace, &probe_input).unwrap()).unwrap();
    let before = detail_resources();
    let cache_before = view.read_profile().0;
    let repeated = serde_json::to_value(view.advice(namespace, &probe_input).unwrap()).unwrap();
    let after = detail_resources();
    let misses = view.read_profile().0 - cache_before;
    assert_eq!(first, repeated);
    assert_eq!(first["len"], 384);
    assert_eq!(
        std::fs::read(directory.path().join("history.index")).unwrap(),
        frozen
    );
    println!(
        "detail-overflow-regression:{}",
        json!({"lookups":129,"logical_bytes":after.0-before.0,"sqlite_cache_misses":misses})
    );
    assert_eq!(
        misses, 0,
        "qualify the warm fixed-key page-residency control"
    );
    // Two SQLite pages allow proc-counter sampling overhead, not another
    // history-sized read budget. Original direct-overflow reads bypass this
    // cache entirely and are invisible to SQLITE_DBSTATUS_CACHE_MISS.
    assert!(
        after.0 - before.0 <= 2 * 4096,
        "warm review-key lookups reread unrelated overflow payloads despite bounded page-cache residency: {} bytes, {misses} cache misses",
        after.0 - before.0
    );
}
