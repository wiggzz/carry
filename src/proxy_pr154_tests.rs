// Included only inside proxy::tests. These exercise existing production trace().
// Deliberately generous hard ceilings: implementations may retain less.
const PR154_RAW_RECORD_MAX: usize = 64 * 1024;
const PR154_RAW_SESSION_MAX: u64 = 256 * 1024;
const PR154_RAW_GLOBAL_MAX: u64 = 1024 * 1024;

fn pr154_log_files(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            files.extend(pr154_log_files(&path));
        } else if path.file_name().unwrap() != ".lock"
            && path.extension().is_none_or(|ext| ext != "json")
        {
            // Include rotations and nested logs, not just the active .jsonl.
            files.push(path);
        }
    }
    files
}

fn pr154_log_bytes(directory: &Path) -> u64 {
    pr154_log_files(directory)
        .iter()
        .map(|path| std::fs::metadata(path).unwrap().len())
        .sum()
}

#[test]
fn pr154_verbose_oversized_record_is_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.verbose = true;
    trace(
        &service,
        "small",
        "primary_received",
        &json!({"fixture": "small-raw-control"}),
    )
    .unwrap();
    assert!(
        pr154_log_files(directory.path()).iter().any(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .contains("small-raw-control")
        }),
        "verbose must still capture a small explicitly opted-in payload"
    );
    trace(
        &service,
        "large",
        "primary_received",
        &json!({"input": "x".repeat(PR154_RAW_RECORD_MAX * 2)}),
    )
    .unwrap();
    for path in pr154_log_files(directory.path()) {
        let text = std::fs::read_to_string(path).unwrap();
        for line in text.lines() {
            assert!(
                line.len() + 1 <= PR154_RAW_RECORD_MAX,
                "PR154: one verbose record must not exceed 64 KiB (including newline); got {}",
                line.len() + 1
            );
            serde_json::from_str::<Value>(line)
                .expect("retained records must remain valid JSON, not byte-truncated fragments");
        }
    }
}

#[test]
fn pr154_verbose_session_retention_survives_restart_without_evicting_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let payload = json!({"input": "x".repeat(16 * 1024)});
    let checkpoint;
    {
        let mut service = checkpoint_service(directory.path());
        service.config.verbose = true;
        save(&service, "lane", &Session::default()).unwrap();
        checkpoint = std::fs::read(directory.path().join("lane.json")).unwrap();
        for _ in 0..10 {
            trace(&service, "lane", "primary_received", &payload).unwrap();
        }
    }
    let mut restarted = checkpoint_service(directory.path());
    restarted.config.verbose = true;
    for _ in 0..10 {
        trace(&restarted, "lane", "primary_received", &payload).unwrap();
    }
    assert_eq!(
        std::fs::read(directory.path().join("lane.json")).unwrap(),
        checkpoint
    );
    assert_eq!(load(&restarted, "lane").unwrap().completed_requests, 0);
    let bytes = pr154_log_bytes(directory.path());
    assert!(
        bytes > 0,
        "verbose capture must not silently become a no-op"
    );
    assert!(
        bytes <= PR154_RAW_SESSION_MAX,
        "PR154: one session's verbose retention must stay within 256 KiB across restart; got {bytes}"
    );
}

#[test]
fn pr154_verbose_global_retention_bounds_many_sessions_across_restart() {
    let directory = tempfile::tempdir().unwrap();
    let payload = json!({"input": "x".repeat(16 * 1024)});
    for batch in 0..2 {
        let mut service = checkpoint_service(directory.path());
        service.config.verbose = true;
        for lane in 0..40 {
            trace(
                &service,
                &format!("lane-{}", batch * 40 + lane),
                "primary_received",
                &payload,
            )
            .unwrap();
        }
    }
    let bytes = pr154_log_bytes(directory.path());
    assert!(
        bytes > 0,
        "verbose capture must not silently become a no-op"
    );
    assert!(
        bytes <= PR154_RAW_GLOBAL_MAX,
        "PR154: many session IDs must share a 1 MiB verbose retention budget across restart; got {bytes}"
    );
}
