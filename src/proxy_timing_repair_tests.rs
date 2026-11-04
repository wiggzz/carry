// Review R1/R2: actual forward/relay owners, synthetic loopback only.
#[test]
fn phase_timing_first_drop_and_service_teardown_do_not_flush_stalled_worker() {
    let _serial = logging::phase_sink_fault::SERIAL.blocking_lock();
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let (fault, entered) = logging::phase_sink_fault::hold(directory.path());
    let (done, completion) = std::sync::mpsc::channel();
    let observer = std::thread::spawn(move || fault.observe(entered, completion));
    let producer = std::thread::spawn(move || {
        drop(timing::Phases::new(
            service.clone(),
            "first-drop-teardown",
            None,
        ));
        drop(service); // Last Service/sender owner, while real sink is stalled.
        let _ = done.send(());
    });
    let prompt = observer.join().unwrap();
    producer.join().unwrap();
    assert!(
        prompt,
        "first phase Drop or Service teardown waited for sink drain"
    );
}

#[test]
fn phase_timing_saturated_queue_drops_observations_without_waiting() {
    let _serial = logging::phase_sink_fault::SERIAL.blocking_lock();
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(checkpoint_service(directory.path()));
    let id = pr154_session_id("bounded-phase-queue");
    let (fault, entered) = logging::phase_sink_fault::hold(directory.path());
    let mut first = timing::Phases::new(service.clone(), "in-flight", None);
    first.session_id = id.clone();
    drop(first);
    // The actual sink worker owns the first observation, before LOG_LOCK/I/O.
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let count = logging::phase_sink_fault::capacity();
    let (done, completion) = std::sync::mpsc::channel();
    let producer = std::thread::spawn({
        let service = service.clone();
        let id = id.clone();
        move || {
            for n in 0..count * 2 {
                let mut guard = timing::Phases::new(service.clone(), &format!("queued-{n}"), None);
                guard.session_id = id.clone();
                drop(guard); // Actual production Drop + bounded try_send.
            }
            let _ = done.send(());
        }
    });
    let prompt = completion.recv_timeout(Duration::from_millis(500)).is_ok();
    fault.release(); // Always release before joining/asserting, including RED.
    drop(fault);
    producer.join().unwrap();
    let events = timing_journal(directory.path(), &id, &format!("queued-{}", count - 1));
    assert!(
        prompt,
        "saturated phase handoff waited for its stalled sink"
    );
    assert_eq!(
        events.len(),
        count + 1,
        "bounded queue must drop overflow observations"
    );
    assert!(events.iter().all(|e| e["event"] == "phase_timing"));
    assert!(
        !events
            .iter()
            .any(|e| e["request_id"] == format!("queued-{count}"))
    );
    let bytes = std::fs::read(
        directory
            .path()
            .join("diagnostics")
            .join(format!("{id}.jsonl")),
    )
    .unwrap();
    assert!(bytes.len() <= 64 * 1024);
    assert!(
        bytes
            .split_inclusive(|b| *b == b'\n')
            .all(|line| line.len() <= 16 * 1024)
    );
}

#[test]
fn phase_timing_unavailable_and_disconnected_sink_fail_open() {
    for disconnected in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut service = checkpoint_service(directory.path());
        service.phase_sink = logging::phase_sink_fault::unavailable(disconnected);
        let service = Arc::new(service);
        let id = pr154_session_id("unavailable-phase-sink");
        journal(&service, &id, "before", "marker", &json!({}));
        let before = pr154_journal(directory.path(), &id)[0]["dropped_before"]
            .as_u64()
            .unwrap();
        let mut guard = timing::Phases::new(service.clone(), "unavailable", None);
        guard.session_id = id.clone();
        drop(guard);
        journal(&service, &id, "after", "marker", &json!({}));
        let events = pr154_journal(directory.path(), &id);
        assert_eq!(
            events.len(),
            2,
            "unavailable observations must not fall back to synchronous I/O"
        );
        assert!(events[1]["dropped_before"].as_u64().unwrap() > before);
        assert_eq!(load(&service, &id).unwrap().primary.calls, 0);
    }
}

#[tokio::test]
async fn phase_timing_precontract_failure_labels_configured_context_unresolved() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.mode = Mode::Audit;
    let service = Arc::new(service);
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "PRIVATE_UNRESOLVED".parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    let response = forward_request(
        service.clone(),
        headers,
        Bytes::from_static(b"not JSON"),
        false,
        "unresolved".into(),
    )
    .await;
    assert_eq!(response.unwrap_err().0, StatusCode::BAD_REQUEST);
    let record = timing_record(directory.path(), &id, "unresolved");
    assert_eq!(record["mode"], "audit");
    assert_eq!(record["data"]["policy_context"], "configured_unresolved");
    assert_eq!(record["data"]["attempts"], json!([]));
    assert_eq!(load(&service, &id).unwrap().primary.calls, 0);
    assert!(!record.to_string().contains("PRIVATE_"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase_timing_stalled_sink_does_not_block_predispatch_cancel() {
    let _serial = logging::phase_sink_fault::SERIAL.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.mode = Mode::Off;
    let (tx, mut waits) = mpsc::unbounded_channel();
    *service.lock_waits.lock().unwrap() = Some(tx);
    let service = Arc::new(service);
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "PRIVATE_CANCEL".parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    let lock = session_lock(&service, &id).await;
    waits.recv().await.unwrap();
    let task = tokio::spawn({
        let service = service.clone();
        async move {
            forward_request(service, headers,
                Bytes::from_static(br#"{"model":"gpt-6-luna","input":[{"role":"user","content":"PRIVATE_PROMPT"}]}"#),
                false, "stalled-cancel".into()).await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), waits.recv())
        .await
        .unwrap()
        .unwrap();
    let (fault, entered) = logging::phase_sink_fault::hold(directory.path());
    let (done, completion) = std::sync::mpsc::channel();
    let observer = std::thread::spawn(move || fault.observe(entered, completion));
    task.abort();
    let cancelled = task.await.unwrap_err().is_cancelled();
    let _ = done.send(());
    let prompt_join = observer.join().unwrap();
    drop(lock);
    assert!(cancelled);
    assert_eq!(load(&service, &id).unwrap().primary.calls, 0);
    let record = timing_record(directory.path(), &id, "stalled-cancel");
    assert_eq!(record["data"]["outcome"], "cancelled");
    assert!(!record.to_string().contains("PRIVATE_"));
    assert!(
        prompt_join,
        "phase sink contention blocked predispatch cancellation join"
    );
}

async fn timing_loopback() -> (
    String,
    mpsc::UnboundedReceiver<Bytes>,
    String,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let reply = json!({"id":"timing-repair","status":"completed","output":[],
        "usage":{"input_tokens":100,"output_tokens":5,"input_tokens_details":{"cached_tokens":0}}})
    .to_string();
    let expected = reply.clone();
    let router = Router::new().route(
        "/responses",
        post(move |bytes: Bytes| {
            let tx = tx.clone();
            let reply = reply.clone();
            async move {
                tx.send(bytes).unwrap();
                ([("content-type", "application/json")], reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, rx, expected, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase_timing_stalled_sink_does_not_hold_relay_eof_or_settlement() {
    let _serial = logging::phase_sink_fault::SERIAL.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let (url, mut captures, expected, server) = timing_loopback().await;
    let mut service = checkpoint_service(directory.path());
    service.config.mode = Mode::Off;
    service.config.upstream_url = url;
    let service = Arc::new(service);
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "PRIVATE_EOF".parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    let bytes = Bytes::from_static(br#"{ "model":"gpt-6-luna", "input":[{"role":"user","content":"PRIVATE_PROMPT"}], "stream":false }"#);
    let (fault, entered) = logging::phase_sink_fault::hold(directory.path());
    let (done, completion) = std::sync::mpsc::channel();
    let observer = std::thread::spawn(move || fault.observe(entered, completion));
    let response = forward_request(
        service.clone(),
        headers,
        bytes.clone(),
        false,
        "stalled-eof".into(),
    )
    .await
    .unwrap();
    let returned = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let _ = done.send(());
    let prompt_eof = observer.join().unwrap();
    assert_eq!(captures.recv().await.unwrap(), bytes);
    assert_eq!(returned, expected);
    let saved = load(&service, &id).unwrap();
    assert_eq!(saved.primary.calls, 1);
    assert_eq!(saved.primary.input_tokens, 100);
    assert_eq!(saved.primary.output_tokens, 5);
    assert_eq!(saved.shadow.calls, 0);
    server.abort();
    let record = timing_record(directory.path(), &id, "stalled-eof");
    assert_eq!(record["data"]["outcome"], "complete");
    assert!(!record.to_string().contains("PRIVATE_"));
    assert!(
        prompt_eof,
        "phase sink contention held client body EOF after settlement"
    );
}

async fn timing_policy_case(mode: Mode, model: &str, capacity: Option<u32>) -> (Value, Vec<Value>) {
    let directory = tempfile::tempdir().unwrap();
    let (url, mut captures, expected, server) = timing_loopback().await;
    let mut service = checkpoint_service(directory.path());
    service.config.mode = mode;
    service.config.upstream_url = url;
    service.config.primary_context_override = capacity;
    service.config.reviewer_context_override = Some(32000);
    // This fixture tests contract attribution, not reviewer cadence.
    service.config.review_replayed_history = false;
    service.config.review_min_primary_tokens = u32::MAX;
    let service = Arc::new(service);
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", "PRIVATE_POLICY".parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    let bytes = Bytes::from(
        json!({"model":model,"input":[{"role":"user","content":"PRIVATE_PROMPT"}],"stream":false})
            .to_string(),
    );
    let response = forward_request(
        service.clone(),
        headers,
        bytes.clone(),
        false,
        "effective-policy".into(),
    )
    .await
    .unwrap();
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        captures.recv().await.unwrap(),
        bytes,
        "complete forwarded wire unchanged"
    );
    assert!(captures.try_recv().is_err(), "no reviewer request");
    let saved = load(&service, &id).unwrap();
    assert_eq!(saved.primary.calls, 1);
    assert_eq!(saved.primary.input_tokens, 100);
    assert_eq!(saved.primary.output_tokens, 5);
    assert_eq!(saved.shadow.calls, 0);
    if model == "unknown-timing-model" {
        assert_eq!(saved.last_plan["reason"], "unsupported_model_or_tier");
        assert_eq!(saved.primary.unavailable_cost_calls, 1);
    } else {
        assert_eq!(
            saved
                .model_resolution
                .as_ref()
                .unwrap()
                .primary
                .capacity_tokens,
            capacity
        );
        assert_eq!(saved.primary.unavailable_cost_calls, 0);
        assert!(saved.primary.cost_usd > 0.0);
    }
    let timing = timing_record(directory.path(), &id, "effective-policy");
    let events = pr154_journal(directory.path(), &id);
    server.abort();
    assert!(!timing.to_string().contains("PRIVATE_"));
    (timing, events)
}

fn assert_timing_effective_policy(timing: &Value, events: &[Value], mode: &str) {
    assert_eq!(timing["data"]["policy_context"], "effective");
    for name in ["primary_received", "primary_submitted", "primary_committed"] {
        let records: Vec<_> = events.iter().filter(|e| e["event"] == name).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["mode"], mode);
        assert_eq!(
            timing["mode"], records[0]["mode"],
            "phase mode differs from actual forwarding owner"
        );
        assert_eq!(
            timing["policy_id"], records[0]["policy_id"],
            "phase policy differs from actual forwarding owner"
        );
    }
}

#[tokio::test]
async fn phase_timing_unsupported_contract_uses_effective_off_context() {
    // Complete all wire/receipt controls for both configured review modes first.
    let compact = timing_policy_case(Mode::Compact, "unknown-timing-model", None).await;
    let audit = timing_policy_case(Mode::Audit, "unknown-timing-model", Some(50000)).await;
    assert_timing_effective_policy(&compact.0, &compact.1, "off");
    assert_timing_effective_policy(&audit.0, &audit.1, "off");
}

#[tokio::test]
async fn phase_timing_resolved_capacity_policy_matches_effective_forward_owner() {
    let (timing, events) = timing_policy_case(Mode::Audit, "gpt-6-luna", Some(50000)).await;
    assert_timing_effective_policy(&timing, &events, "audit");
}
