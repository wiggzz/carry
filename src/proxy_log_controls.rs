// Existing logging/privacy controls retained independently of removed replay mechanisms.
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
    let id = pr154_session_id("checkpoint");
    let raw = directory.path().join(format!("{id}.jsonl"));
    std::fs::write(&raw, b"old opted-in evidence\n").unwrap();
    let service = checkpoint_service(directory.path());
    save(&service, &id, &Session::default()).unwrap();
    let checkpoint = std::fs::read(directory.path().join(format!("{id}.json"))).unwrap();
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
                            &pr154_session_id(&format!("lane-{lane}")),
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
                assert!(line.len() < 16 * 1024);
                serde_json::from_str::<Value>(line).unwrap();
            }
        }
    }
    assert_eq!(std::fs::read(&raw).unwrap(), b"old opted-in evidence\n");
    assert_eq!(
        std::fs::read(directory.path().join(format!("{id}.json"))).unwrap(),
        checkpoint
    );
}

#[test]
fn pr154_journal_oversize_io_and_partial_restart_are_fail_open() {
    let directory = tempfile::tempdir().unwrap();
    let id = pr154_session_id("lane");
    let service = checkpoint_service(directory.path());
    journal(&service, &id, "request", "small", &json!({"count":1}));
    let path = directory
        .path()
        .join("diagnostics")
        .join(format!("{id}.jsonl"));
    let before = std::fs::read(&path).unwrap();
    journal(
        &service,
        &id,
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
    journal(&service, &id, "request", "restarted", &json!({}));
    let events = pr154_journal(directory.path(), &id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "restarted");
    assert!(events[0]["dropped_before"].as_u64().unwrap() > 0);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    journal(&service, &id, "request", "io_failure", &json!({}));
    save(&service, &id, &Session::default()).unwrap();
    assert_eq!(load(&service, &id).unwrap().completed_requests, 0);
    assert!(path.is_dir());
}

#[test]
fn pr154_verbose_legacy_maintenance_never_touches_checkpoint_or_symlink() {
    let directory = tempfile::tempdir().unwrap();
    let id = pr154_session_id("lane");
    let link_id = pr154_session_id("link");
    let mut service = checkpoint_service(directory.path());
    save(&service, &id, &Session::default()).unwrap();
    let checkpoint = std::fs::read(directory.path().join(format!("{id}.json"))).unwrap();
    // Legacy production traces already used identity() hashes; legacy.jsonl
    // would instead represent an unrelated artifact outside raw ownership.
    let raw = directory.path().join(format!("{id}.jsonl"));
    let oversized = json!({"input":"x".repeat(100 * 1024)}).to_string() + "\n";
    std::fs::write(&raw, oversized.as_bytes()).unwrap();
    let temporary = directory.path().join(format!(".{id}.tmp"));
    std::fs::write(&temporary, b"checkpoint temporary").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        directory.path().join(format!("{id}.json")),
        directory.path().join(format!("{link_id}.jsonl")),
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
        std::fs::read(directory.path().join(format!("{id}.json"))).unwrap(),
        checkpoint
    );
    assert_eq!(std::fs::read(temporary).unwrap(), b"checkpoint temporary");
    #[cfg(unix)]
    assert!(
        std::fs::symlink_metadata(directory.path().join(format!("{link_id}.jsonl")))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn pr154_raw_retention_and_verbose_restart_preserve_independent_jsonl() {
    let directory = tempfile::tempdir().unwrap();
    let legacy_id = pr154_session_id("pr154-legacy-trace");
    let active_id = pr154_session_id("pr154-active-trace");
    let legacy = directory.path().join(format!("{legacy_id}.jsonl"));
    let active = directory.path().join(format!("{active_id}.jsonl"));
    // Valid, small JSON records exceed the raw lane budget only in aggregate.
    let record = json!({"padding":"x".repeat(1024)}).to_string() + "\n";
    let bytes = record.repeat(300).into_bytes();
    assert!(bytes.len() > 256 * 1024);
    let artifacts = [
        ("benchmark-events.jsonl".to_owned(), bytes.as_slice()),
        ("unrelated.jsonl".to_owned(), bytes.as_slice()),
        (format!("{}.jsonl", "a".repeat(63)), bytes.as_slice()),
        (format!("{}.jsonl", "a".repeat(65)), bytes.as_slice()),
        (format!("{}.jsonl", "A".repeat(64)), bytes.as_slice()),
        (format!("{}.jsonl", "g".repeat(64)), bytes.as_slice()),
        // Below all byte caps: only restart's JSON/partial-line validation
        // could destroy these independently owned artifacts.
        ("partial.jsonl".to_owned(), b"{\"partial\":".as_slice()),
        ("invalid.jsonl".to_owned(), b"not JSON\n".as_slice()),
    ]
    .map(|(name, expected)| (directory.path().join(name), expected));
    let mut observations = Vec::new();
    for restart in [false, true] {
        // Reseed every artifact so restart runs even if append deletes one.
        for (path, expected) in &artifacts {
            std::fs::write(path, expected).unwrap();
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
            artifacts
                .each_ref()
                .map(|(path, _)| std::fs::read(path).ok()),
        ));
    }
    for (restart, observed) in observations {
        for ((path, expected), actual) in artifacts.iter().zip(observed) {
            assert!(
                actual.as_deref() == Some(*expected),
                "independent artifact {} must be byte-preserved (restart={restart})",
                path.display()
            );
        }
    }
}

#[test]
fn pr154_usage_diagnostics_are_numeric_and_preserve_censoring() {
    let directory = tempfile::tempdir().unwrap();
    let id = pr154_session_id("lane");
    let service = checkpoint_service(directory.path());
    for response in [
        json!({"status":"PRIVATE_STATUS","service_tier":"PRIVATE_TIER","usage":{"input_tokens":10,"output_tokens":1,"input_tokens_details":{"cached_tokens":0,"private":"PRIVATE_USAGE"},"private":"PRIVATE_USAGE"},"output":[{"name":"PRIVATE_TOOL"}]}),
        json!({"status":"completed","usage":{"input_tokens":10,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}}),
        json!({"status":"completed","usage":{"input_tokens":10,"output_tokens":1}}),
        json!({"status":"completed","usage":{"input_tokens":"PRIVATE_USAGE"}}),
    ] {
        logging::usage(
            &service,
            &id,
            "request",
            "primary",
            Some(&response),
            &json!({"model":"gpt-6-luna"}),
            true,
        );
    }
    let events = pr154_journal(directory.path(), &id);
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

// R2: sink-local ceilings are not authority to spend a paid owner's promise.
#[test]
fn diagnostic_writers_leave_paid_and_maintenance_promises_intact() {
    let mut violations = Vec::new();
    for raw in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.verbose = true;
        service.config.history_max_bytes = 65536;
        service.config.history_disk_bytes = 8 * 1024 * 1024;
        let id = pr154_session_id("diagnostic-promise");
        let mut session = Session {
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        numeric_primary(&mut session, 0);
        session.primary_pending = true;
        save(&service, &id, &session).unwrap();
        let pin = history::pin(&service, &id).unwrap();
        // Pre-create the diagnostic directory through the normal writer, before
        // filling ordinary headroom. This isolates append from mkdir admission.
        journal(&service, &id, "setup", "fixture", &json!({}));
        let policy = history::FilePolicy::new(&service.config);
        let publication = policy.acquire().unwrap();
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
        assert!(publication.admit_file(directory.path(), 0).is_ok());
        assert!(publication.admit_file(directory.path(), 1).is_err());
        drop(publication);
        let data = json!({"counts":vec![123u64; 1000]});
        if raw {
            logging::raw(&service, &id, "fixture", &data);
        } else {
            journal(&service, &id, "request", "fixture", &data);
        }
        let publication = policy.acquire().unwrap();
        let promises_intact = publication.admit_file(directory.path(), 0).is_ok();
        drop(publication);
        // Complete the genuinely captured native receipt even on the old bad
        // writer, then verify cold monetary/count/pending controls before RED.
        numeric_primary(&mut session, 1);
        session.primary_pending = false;
        save_paid_accounting(&service, &id, &session, "diagnostic-promise").unwrap();
        drop(pin);
        let cold = checkpoint_service(directory.path());
        let restored = load(&cold, &id).unwrap();
        assert_eq!(restored.primary.calls, 2);
        assert!((restored.primary.cost_usd - 0.004).abs() < 1e-12);
        assert!(!restored.primary_pending);
        assert_eq!(restored.completed_requests, 2);
        if !promises_intact {
            violations.push(if raw { "raw" } else { "journal" });
        }
    }
    assert!(
        violations.is_empty(),
        "diagnostic writers consumed protected paid/maintenance capacity: {violations:?}"
    );
}

#[test]
fn diagnostic_writers_cannot_race_a_held_physical_publication() {
    let mut raced = Vec::new();
    for raw in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.verbose = true;
        let id = pr154_session_id("diagnostic-race");
        journal(&service, &id, "setup", "fixture", &json!({}));
        let publication = history::FilePolicy::new(&service.config).acquire().unwrap();
        let (started, start) = std::sync::mpsc::channel();
        let (done, finish) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                started.send(()).unwrap();
                if raw {
                    logging::raw(&service, &id, "fixture", &json!({"count":1}));
                } else {
                    journal(&service, &id, "request", "fixture", &json!({"count":1}));
                }
                done.send(()).unwrap();
            });
            start.recv_timeout(Duration::from_secs(5)).unwrap();
            let completed_while_held = finish.recv_timeout(Duration::from_millis(500)).is_ok();
            drop(publication); // Always release real ownership before grading.
            if !completed_while_held {
                finish.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            worker.join().unwrap();
            if completed_while_held {
                raced.push(if raw { "raw" } else { "journal" });
            }
        });
    }
    assert!(
        raced.is_empty(),
        "writers bypassed held physical owner: {raced:?}"
    );
}

#[test]
fn queued_phase_writer_uses_dispatch_policy_without_spending_physical_promises() {
    let _serial = logging::phase_sink_fault::SERIAL.blocking_lock();
    for pressured in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.history_max_bytes = 65536;
        service.config.history_disk_bytes = 8 * 1024 * 1024;
        let id = pr154_session_id("phase-promise");
        let mut session = Session {
            request_evidence: Some(crate::proxy_savings::RequestEvidence::default()),
            ..Session::default()
        };
        numeric_primary(&mut session, 0);
        session.primary_pending = true;
        save(&service, &id, &session).unwrap();
        let pin = history::pin(&service, &id).unwrap();
        let policy = history::FilePolicy::new(&service.config);
        if pressured {
            let publication = policy.acquire().unwrap();
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
            assert!(publication.admit_file(directory.path(), 0).is_ok());
        }
        let (fault, entered) = logging::phase_sink_fault::hold(directory.path());
        logging::phase_summary(&service, &id, "queued", None, &json!({"count":1}));
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        // A queued observation owns its original policy, not whichever service
        // config would happen to be current when its filesystem writer runs.
        service.config.history_disk_bytes = if pressured { 16 * 1024 * 1024 } else { 65536 };
        fault.release();
        assert!(fault.wait_appended(Duration::from_secs(5)));
        drop(fault);
        service.config.history_disk_bytes = 8 * 1024 * 1024;
        let path = directory
            .path()
            .join("diagnostics")
            .join(format!("{id}.jsonl"));
        assert_eq!(path.is_file(), !pressured);
        assert_eq!(
            directory.path().join("diagnostics").exists(),
            !pressured,
            "refused phase must not create an unadmitted directory"
        );
        if !pressured {
            assert_eq!(
                pr154_journal(directory.path(), &id)[0]["event"],
                "phase_timing"
            );
        }
        assert!(
            policy
                .acquire()
                .unwrap()
                .admit_file(directory.path(), 0)
                .is_ok()
        );
        numeric_primary(&mut session, 1);
        session.primary_pending = false;
        save_paid_accounting(&service, &id, &session, "phase-promise").unwrap();
        drop(pin);
        let restored = load(&checkpoint_service(directory.path()), &id).unwrap();
        assert_eq!(restored.primary.calls, 2);
        assert!((restored.primary.cost_usd - 0.004).abs() < 1e-12);
        assert!(!restored.primary_pending);
    }
}

#[test]
fn queued_phase_writer_waits_for_physical_owner_but_enqueue_does_not() {
    let _serial = logging::phase_sink_fault::SERIAL.blocking_lock();
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    let id = pr154_session_id("phase-publication-race");
    let (fault, entered) = logging::phase_sink_fault::hold(directory.path());
    let publication = history::FilePolicy::new(&service.config).acquire().unwrap();
    logging::phase_summary(&service, &id, "queued", None, &json!({"count":1}));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    fault.release();
    let raced = fault.wait_appended(Duration::from_millis(500));
    let wrote_while_held = directory.path().join("diagnostics").exists();
    drop(publication);
    assert!(fault.wait_appended(Duration::from_secs(5)));
    drop(fault);
    assert!(
        !raced && !wrote_while_held,
        "phase worker bypassed held physical owner"
    );
    assert_eq!(
        pr154_journal(directory.path(), &id)[0]["event"],
        "phase_timing"
    );
}

#[test]
fn diagnostic_maintenance_does_not_create_missing_directory() {
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    logging::maintain(&service);
    assert!(!directory.path().join("diagnostics").exists());
}
