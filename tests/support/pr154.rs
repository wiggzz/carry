//! Tests-only PR154 loopback regressions; reuse the existing native harness.
use super::*;
use axum::{Router, routing::post};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

fn journal_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            files.extend(journal_files(&entry.path()));
        } else if entry.path().extension().is_some_and(|ext| ext == "jsonl") {
            files.push(entry.path());
        }
    }
    files.sort();
    files
}

async fn default_failure_journal(mode: &str) {
    let failures = Arc::new(Mutex::new(std::collections::VecDeque::from([
        json!({"error": {"type": "invalid_request_error", "code": "string_above_max_length", "param": "prompt_cache_key", "message": "PRIVATE_PROVIDER_MESSAGE", "request": "PRIVATE_PROVIDER_REQUEST"}}),
        json!({"error": {"type": "invalid_request_error", "code": "context_length_exceeded", "param": "input", "message": "PRIVATE_PROVIDER_MESSAGE"}}),
        json!({"error": {"type": "PRIVATE TYPE WITH SPACES", "code": "x".repeat(65), "param": "PRIVATE\nPARAM", "message": "PRIVATE_PROVIDER_MESSAGE"}}),
    ])));
    let remaining = failures.clone();
    let router = Router::new()
        .route("/v1/responses", post(|| async {
            axum::Json(json!({"status":"completed", "output":[], "usage":{
                "input_tokens":100, "output_tokens":1, "input_tokens_details":{"cached_tokens":0}}}))
        }))
        .route("/review", post(move || {
            let failures = failures.clone();
            async move {
                (axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(failures.lock().await.pop_front().expect("only three reviews expected")))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let request = json!({"model":"gpt-6-luna", "input":[
        {"role":"user", "content":"PRIVATE_TASK"},
        {"type":"function_call", "call_id":"a", "name":"native", "arguments":"{}"},
        {"type":"function_call_output", "call_id":"a", "output":"PRIVATE_TOOL_OUTPUT"},
        {"role":"user", "content":"PRIVATE_CURRENT_TASK"}
    ]});
    // Two requests, restart, two more: the first failure must remain available
    // even after a later failure replaces the checkpoint's latest feedback.
    for _ in 0..2 {
        let (_proxy, url) = start_proxy(state.path(), address, mode).await;
        for _ in 0..2 {
            let response = send(&url, &request, "pr154-owner", "main").await;
            assert!(
                response.status().is_success(),
                "review failure must not fail primary"
            );
            response.bytes().await.unwrap();
        }
    }
    server.abort();
    assert!(
        remaining.lock().await.is_empty(),
        "fixture must observe all three actual HTTP failures"
    );
    let saved = checkpoint_states(state.path()).remove(0);
    assert_eq!(saved["completed_requests"], 4);
    assert_eq!(saved["invalid_reviews"], 3);
    assert_eq!(saved["shadow"]["unavailable_cost_calls"], 3);
    assert_eq!(saved["compactions"], 0);
    let mut entries = Vec::new();
    let mut bytes = 0;
    for path in journal_files(state.path()) {
        let text = std::fs::read_to_string(path).unwrap();
        bytes += text.len();
        for forbidden in [
            "PRIVATE_",
            "fixture-primary",
            "fixture-shadow",
            "pr154-owner",
        ] {
            assert!(
                !text.contains(forbidden),
                "default diagnostics leaked {forbidden}"
            );
        }
        for line in text.lines() {
            assert!(
                line.len() < 16 * 1024,
                "default diagnostic record must be bounded"
            );
            entries.push(serde_json::from_str::<Value>(line).unwrap());
        }
    }
    assert!(
        bytes <= 1024 * 1024,
        "default journal must have a global byte ceiling"
    );
    let failed = entries
        .iter()
        .filter(|e| e["event"] == "shadow_failed")
        .collect::<Vec<_>>();
    assert_eq!(
        failed.len(),
        3,
        "PR154: default mode must retain every bounded reviewer failure across restart without verbose payload capture"
    );
    let failures = failed.iter().map(|e| e["data"].clone()).collect::<Vec<_>>();
    for code in ["string_above_max_length", "context_length_exceeded"] {
        assert_eq!(
            failures.iter().filter(|f| f["error_code"] == code).count(),
            1
        );
    }
    for failure in &failures {
        assert_eq!(failure["reason"], "review_transport_or_protocol_failure");
        assert_eq!(failure["http_status"], 400);
        assert!(
            failure.as_object().unwrap().keys().all(|key| [
                "reason",
                "http_status",
                "error_type",
                "error_code",
                "error_param"
            ]
            .contains(&key.as_str())),
            "never log arbitrary provider fields"
        );
    }
    assert!(
        failures.iter().any(|f| f.get("error_code").is_none()
            && f.get("error_type").is_none()
            && f.get("error_param").is_none()),
        "invalid provider identifiers must be omitted, not truncated into apparently safe values"
    );
    let checkpoint_id = std::fs::read_dir(state.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap()
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let mut requests = std::collections::HashSet::new();
    for event in failed {
        assert_eq!(event["session_id"], checkpoint_id);
        assert_eq!(event["mode"], mode);
        assert!(event["at"].as_u64().is_some());
        let request_id = event["request_id"]
            .as_str()
            .expect("correlate to the triggering primary request");
        assert!(!request_id.is_empty());
        assert!(
            requests.insert(request_id.to_owned()),
            "separate requests, including after restart, need distinct correlation IDs"
        );
        assert!(
            entries.iter().any(|plan| plan["event"] == "paired_plan"
                && plan["session_id"] == event["session_id"]
                && plan["request_id"] == event["request_id"]),
            "keep the decision timeline, not just the latest checkpoint plan"
        );
    }
}

#[tokio::test]
async fn pr154_compact_default_preserves_sanitized_failure_timeline_after_restart() {
    default_failure_journal("compact").await;
}

#[tokio::test]
async fn pr154_audit_default_preserves_sanitized_failure_timeline_after_restart() {
    default_failure_journal("audit").await;
}

#[tokio::test]
async fn pr154_verbose_trace_io_failure_does_not_fail_inference_or_checkpoint() {
    let calls = Arc::new(Mutex::new(0));
    let observed = calls.clone();
    let router = Router::new().route(
        "/v1/responses",
        post(move || {
            let calls = calls.clone();
            async move {
                *calls.lock().await += 1;
                axum::Json(json!({"id":"resp_pr154", "status":"completed", "output":[],
                "usage":{"input_tokens":7,"output_tokens":3}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let request = json!({"model":"fixture-model", "input":[{"role":"user","content":"hello"}]});
    {
        let (_proxy, url) =
            start_proxy_options(state.path(), address, "off", &["--verbose"], "").await;
        let response = send(&url, &request, "pr154-owner", "main").await;
        assert!(response.status().is_success());
        response.bytes().await.unwrap();
    }
    // A directory occupying the existing raw trace path gives a deterministic
    // EISDIR even under root, while leaving checkpoint writes fully writable.
    let checkpoint = std::fs::read_dir(state.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let raw = checkpoint.with_extension("jsonl");
    assert!(
        raw.is_file(),
        "verbose control must create its session trace"
    );
    std::fs::remove_file(&raw).unwrap();
    std::fs::create_dir(&raw).unwrap();
    let (_proxy, url) = start_proxy_options(state.path(), address, "off", &["--verbose"], "").await;
    let response = send(&url, &request, "pr154-owner", "main").await;
    let status = response.status();
    let body = response.bytes().await.unwrap();
    server.abort();
    assert!(
        status.is_success(),
        "PR154: trace I/O is fail-open, not an inference HTTP500: {status} {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["id"],
        "resp_pr154"
    );
    assert_eq!(
        *observed.lock().await,
        2,
        "the valid second primary must actually reach the provider"
    );
    let saved = checkpoint_states(state.path()).remove(0);
    assert_eq!(saved["completed_requests"], 2);
    assert_eq!(saved["primary"]["input_tokens"], 14);
}
