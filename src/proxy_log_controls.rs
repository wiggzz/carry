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
            "gpt-6-luna",
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
