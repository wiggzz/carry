use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "support/admission.rs"]
mod admission;
#[path = "support/pr154.rs"]
mod pr154;

#[test]
fn transparent_server_owned_history() {
    let output = Command::new("python3")
        .args([
            "-B",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/proxy_transparent_history.py"
            ),
            "-v",
        ])
        .env("CARRY_TEST_BINARY", env!("CARGO_BIN_EXE_carry"))
        .output()
        .expect("transparent loopback history regression");
    std::io::Write::write_all(&mut std::io::stdout(), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Proxy(Child);

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// One stdlib HTTP fixture is shared by CI's built binary and the immutable
// prebuilt baseline. This lets model-free RED evidence execute the exact tests
// without compiling a second implementation of the regression locally.
fn state_regression(selector: &str) {
    let evidence = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg("-B")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/proxy_state_regressions.py"
        ))
        .arg(format!("StateRegressions.test_{selector}"))
        .arg("-v")
        .env("CARRY_TEST_BINARY", env!("CARGO_BIN_EXE_carry"))
        .env("CARRY_TEST_EVIDENCE_DIR", evidence.path())
        .output()
        .expect("Python 3 must run the loopback-only state regression fixture");
    assert!(
        output.status.success(),
        "state regression {selector} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if selector.starts_with("capacity_rejection_") {
        std::io::Write::write_all(&mut std::io::stdout(), &output.stdout).unwrap();
    }
    if selector.starts_with("small_") {
        let path = evidence.path().join(format!("test_{selector}.json"));
        let captured: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let limits = &captured["reviewer_limits"];
        for body in captured["reviewer_requests"].as_array().unwrap() {
            let rendered = body.to_string();
            assert!(rendered.len() as u64 <= limits["bytes"].as_u64().unwrap());
            let tokens = tiktoken_rs::o200k_base_singleton()
                .encode_ordinary(&rendered)
                .len();
            assert!(
                tokens as u64 <= limits["tokens"].as_u64().unwrap(),
                "S3: actual subsequent reviewer dispatch must remain within its configured o200k_base token estimate"
            );
        }
    }
}

#[test]
fn reviewer_control_observations_once_and_restart() {
    state_regression("reviewer_control_observations_once_and_restart");
}

#[test]
fn direction_changes_reject_held_advice_and_preserve_paid_receipt() {
    state_regression("direction_changes_reject_held_advice_and_preserve_paid_receipt");
}

#[test]
fn capacity_rejection_unchanged_direction_accepts_held_advice() {
    state_regression("capacity_rejection_unchanged_direction_accepts_held_advice");
}

#[test]
fn capacity_rejection_changed_direction_rejects_held_advice() {
    state_regression("capacity_rejection_changed_direction_rejects_held_advice");
}

#[test]
fn capacity_rejection_cleared_direction_rejects_held_advice() {
    state_regression("capacity_rejection_cleared_direction_rejects_held_advice");
}

#[test]
fn capacity_rejection_clear_restore_rejects_held_advice() {
    state_regression("capacity_rejection_clear_restore_rejects_held_advice");
}

#[test]
fn capacity_rejection_failed_save_unchanged_accepts_held_advice() {
    state_regression("capacity_rejection_failed_save_unchanged_accepts_held_advice");
}

#[test]
fn capacity_rejection_failed_save_changed_rejects_held_advice() {
    state_regression("capacity_rejection_failed_save_changed_rejects_held_advice");
}

#[test]
fn capacity_rejection_failed_save_cleared_rejects_held_advice() {
    state_regression("capacity_rejection_failed_save_cleared_rejects_held_advice");
}

#[test]
fn capacity_rejection_failed_save_restore_rejects_held_advice() {
    state_regression("capacity_rejection_failed_save_restore_rejects_held_advice");
}

#[test]
fn reviewer_wire_fingerprints_are_content_free_and_join_usage() {
    state_regression("reviewer_wire_fingerprints_are_content_free_and_join_usage");
}

#[test]
fn dashboard_backend_authoritative_savings() {
    state_regression("dashboard_backend_authoritative_savings");
}

#[test]
fn priced_subset_primary_omissions_and_restart() {
    state_regression("priced_subset_primary_omissions_and_restart");
}

#[test]
fn priced_subset_all_unpriced_and_aggregate_counters() {
    state_regression("priced_subset_all_unpriced_and_aggregate_counters");
}

#[test]
fn successful_request_savings_after_failure_and_restart() {
    state_regression("successful_request_savings_after_failure_and_restart");
}

#[test]
fn traditional_summary_between_tool_requests_survives_restart() {
    state_regression("traditional_summary_between_tool_requests_survives_restart");
}

#[test]
fn legacy_checkpoint_keeps_fail_closed_savings_policy() {
    state_regression("legacy_checkpoint_keeps_fail_closed_savings_policy");
}

#[test]
fn failed_primary_terminals_do_not_mutate_successful_scenario() {
    state_regression("failed_primary_terminals_do_not_mutate_successful_scenario");
}

#[test]
fn reviewer_completion_classification_survives_background_merge_and_restart() {
    state_regression("reviewer_completion_classification_survives_background_merge_and_restart");
}

#[test]
fn successful_reviewer_spend_survives_failed_primary_exactly_once() {
    state_regression("successful_reviewer_spend_survives_failed_primary_exactly_once");
}

#[test]
fn restart_orphan_reviewer_is_untracked_not_proven_failed() {
    state_regression("restart_orphan_reviewer_is_untracked_not_proven_failed");
}

#[test]
fn failed_429_continuation_retry_once() {
    state_regression("failed_429_continuation_retry_once");
}

#[test]
fn failed_429_continuation_retry_once_after_restart() {
    state_regression("failed_429_continuation_retry_once_after_restart");
}

#[test]
fn transport_failed_continuation_retry_once() {
    state_regression("transport_failed_continuation_retry_once");
}

#[test]
fn transport_failed_continuation_retry_once_after_restart() {
    state_regression("transport_failed_continuation_retry_once_after_restart");
}

#[test]
fn old_drop_does_not_remove_grown_group_during_held_review() {
    state_regression("old_drop_does_not_remove_grown_group_during_held_review");
}

#[test]
fn small_memory_budget_rejects_large_fact_preserves_small_sibling_and_opinions() {
    state_regression("small_memory_budget_rejects_large_fact_preserves_small_sibling_and_opinions");
}

#[test]
fn small_byte_budget_memory_admission_preserves_review_progress() {
    state_regression("small_byte_budget_memory_admission_preserves_review_progress");
}

#[test]
fn small_token_budget_memory_admission_preserves_review_progress() {
    state_regression("small_token_budget_memory_admission_preserves_review_progress");
}

#[test]
fn incremental_reviewer_context_regressions() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/proxy_incremental.py"
        ))
        .arg("-v")
        .env("CARRY_TEST_BINARY", env!("CARGO_BIN_EXE_carry"))
        .output()
        .expect("Python 3 must run the actual-binary incremental reviewer fixture");
    assert!(
        output.status.success(),
        "incremental reviewer regression failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reviewer_independent_primary_http_regressions() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/proxy_independence.py"
        ))
        .arg("-v")
        .env("CARRY_TEST_BINARY", env!("CARGO_BIN_EXE_carry"))
        .output()
        .expect("Python 3 must execute reviewer-independent primary HTTP regressions");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// Source-bound PR144 behavior, separate from the unchanged incremental/5s
// timeout regression. The same file runs against the provenance-bound baseline.
#[test]
fn pr144_review_behavior_regressions() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/proxy_review_regressions.py"
        ))
        .arg("-v")
        .env("CARRY_TEST_BINARY", env!("CARGO_BIN_EXE_carry"))
        .output()
        .expect("Python 3 must execute source-bound PR144 loopback regressions");
    assert!(
        output.status.success(),
        "PR144 behavioral regression failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn proxy_cli_serves_health_and_forwards_opaque_json() {
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (mut proxy, url) = start_proxy(state.path(), upstream_address, "off").await;
    let address = url.strip_prefix("http://").unwrap();
    let client = reqwest::Client::new();
    let mut healthy = false;
    for _ in 0..100 {
        if let Ok(response) = client.get(format!("http://{address}/health")).send().await {
            healthy = response.status().is_success();
            if healthy {
                break;
            }
        }
        if let Some(status) = proxy.0.try_wait().unwrap() {
            panic!("missing proxy frontdoor: carry proxy exited {status}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        healthy,
        "carry proxy must expose a working HTTP health route"
    );
    let input = json!({
        "model": "fixture-model",
        "input": [{"role": "user", "content": "opaque user's request"}],
        "tools": [{"type": "function", "name": "native", "parameters": {"type": "object"}}],
        "reasoning": {"effort": "medium"},
        "store": false
    });
    let expected = input.clone();
    let output = br#"{ "id":"resp_fixture", "status":"completed", "output":[{"type":"unknown_native","opaque":"preserve"}], "usage":{"input_tokens":7,"output_tokens":3} }"#;
    let provider = tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let count = socket.read(&mut buf).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buf[..count]);
            if let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..split]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|value| value.parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= split + 4 + length {
                    let body: serde_json::Value =
                        serde_json::from_slice(&bytes[split + 4..split + 4 + length]).unwrap();
                    assert_eq!(body, expected, "primary request must remain opaque");
                    assert!(headers.contains("Bearer fixture-primary"));
                    break;
                }
            }
        }
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    output.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(output).await.unwrap();
    });
    let response = client
        .post(format!("http://{address}/v1/responses"))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.bytes().await.unwrap().as_ref(), output);
    provider.await.unwrap();
}

#[tokio::test]
async fn proxy_does_not_write_session_traces_by_default() {
    use axum::{Router, routing::post};
    let router = Router::new().route(
        "/v1/responses",
        post(|| async {
            axum::Json(
                json!({"id": "resp_trace", "status": "completed", "output": [],
            "usage": {"input_tokens": 7, "output_tokens": 3}}),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    for completed in 1..=2 {
        let (_proxy, url) = start_proxy(state.path(), address, "off").await;
        let response = send(
            &url,
            &json!({"model": "fixture-model", "input": [
                {"role": "user", "content": "hello"}
            ]}),
            "trace-test",
            "main",
        )
        .await;
        assert!(response.status().is_success());
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["id"],
            "resp_trace"
        );
        let saved = settled(state.path()).await.remove(0);
        assert_eq!(saved["completed_requests"], completed);
        assert_eq!(saved["primary"]["input_tokens"], completed * 7);
        assert!(
            std::fs::read_dir(state.path()).unwrap().all(|entry| {
                entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_none_or(|ext| ext != "jsonl")
            }),
            "default proxy must not create verbose session traces"
        );
    }
    server.abort();
}

#[tokio::test]
async fn proxy_verbose_traces_streams_and_default_restart_leaves_them_untouched() {
    use axum::{Router, routing::post};
    let terminal = json!({"id": "resp_trace", "status": "completed", "output": [],
        "usage": {"input_tokens": 7, "output_tokens": 3}});
    let wire = format!(
        "event: response.completed\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"type": "response.completed", "response": terminal})
    );
    let expected = wire.clone();
    let router = Router::new().route(
        "/v1/responses",
        post(move || {
            let wire = wire.clone();
            async move { ([("content-type", "text/event-stream")], wire) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let mut previous_trace = None;
    // A default restart must neither append to nor delete an existing verbose trace.
    for verbose in [true, false] {
        let extra = if verbose { vec!["--verbose"] } else { vec![] };
        let (_proxy, url) = start_proxy_options(state.path(), address, "off", &extra, "").await;
        let response = send(
            &url,
            &json!({"model": "fixture-model", "stream": true, "input": [
                {"role": "user", "content": "hello"}
            ]}),
            "trace-test",
            "main",
        )
        .await;
        assert!(response.status().is_success());
        assert_eq!(response.text().await.unwrap(), expected);
        let saved = settled(state.path()).await.remove(0);
        assert_eq!(saved["completed_requests"], if verbose { 1 } else { 2 });
        let trace_path = std::fs::read_dir(state.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .unwrap();
        let trace = std::fs::read_to_string(trace_path).unwrap();
        if let Some(previous) = &previous_trace {
            assert_eq!(
                &trace, previous,
                "default mode must leave existing logs untouched"
            );
        } else {
            let events: Vec<serde_json::Value> = trace
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            for event in [
                "primary_received",
                "primary_submitted",
                "primary_response_chunk",
                "primary_completed",
            ] {
                assert!(
                    events.iter().any(|entry| entry["event"] == event),
                    "missing {event}"
                );
            }
            let received = events
                .iter()
                .find(|entry| entry["event"] == "primary_received")
                .unwrap();
            assert_eq!(received["data"]["input"][0]["content"], "hello");
            previous_trace = Some(trace);
        }
    }
    server.abort();
}

#[tokio::test]
async fn v2_trigger_is_a_native_checkpoint_not_an_ordinary_generation() {
    use axum::{Router, routing::post};
    let checkpoint =
        json!({"type": "compaction", "id": "cmp_v2", "encrypted_content": "FIXTURE_ONLY"});
    let returned = checkpoint.clone();
    let router = Router::new().route("/v1/responses", post(move |axum::Json(body): axum::Json<serde_json::Value>| {
        let checkpoint = returned.clone();
        async move {
            assert_eq!(body["input"].as_array().unwrap().last().unwrap()["type"], "compaction_trigger");
            let output = if body["metadata"]["invalid"] == true { json!([]) } else { json!([checkpoint]) };
            axum::Json(json!({"status": "completed", "output": output, "usage": {"input_tokens": 100, "output_tokens": 10, "input_tokens_details": {"cached_tokens": 0}}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(temp.path(), address, "compact").await;
    let client = reqwest::Client::new();
    let body = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "goal"}, {"type": "compaction_trigger"}]});
    let response: serde_json::Value = client
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "v2")
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["output"][0], checkpoint);
    let metrics: serde_json::Value = client
        .get(format!("{url}/carry/metrics"))
        .header("x-carry-session", "v2")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        metrics["native_compactions"], 1,
        "V2 must retire both active projections and account as native compaction"
    );
    assert_eq!(
        metrics["completed_requests"], 0,
        "a checkpoint is not completed main exposure"
    );
    let invalid = json!({"model": "gpt-6-luna", "input": [checkpoint, {"type": "compaction_trigger"}], "metadata": {"invalid": true}});
    let response: serde_json::Value = client
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "v2")
        .json(&invalid)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        response["output"],
        json!([]),
        "do not fabricate an upstream checkpoint"
    );
    let metrics: serde_json::Value = client
        .get(format!("{url}/carry/metrics"))
        .header("x-carry-session", "v2")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        metrics["native_compactions"], 1,
        "completed without exactly one opaque anchor is not native checkpoint success"
    );
    assert_eq!(metrics["failed_primaries"], 1);
    server.abort();
}

#[tokio::test]
async fn explicit_non_prefix_rebase_retires_both_projections_without_inventing_ancestry() {
    use axum::{Router, routing::post};
    let router = Router::new().route("/v1/responses", post(|axum::Json(body): axum::Json<serde_json::Value>| async move {
        axum::Json(json!({"status": "completed", "output": [], "usage": {"input_tokens": 100, "output_tokens": 1, "input_tokens_details": {"cached_tokens": 0}}, "echo": body["input"]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(temp.path(), address, "compact").await;
    let client = reqwest::Client::new();
    for goal in ["old active source", "caller installed summary"] {
        let body = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": goal}]});
        let response = client
            .post(format!("{url}/v1/responses"))
            .header("x-carry-session", "rebase")
            .header("x-carry-history-policy", "reset-on-divergence")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "explicit reset must accept replacement without resurrecting source"
        );
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(value["echo"], body["input"]);
    }
    let metrics: serde_json::Value = client
        .get(format!("{url}/carry/metrics"))
        .header("x-carry-session", "rebase")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metrics["history_rebases"], 1);
    assert_eq!(
        metrics["primary"]["calls"], 2,
        "rebase retains immutable billing counters"
    );
    server.abort();
}

#[tokio::test]
async fn off_forwards_non_prefix_and_stateful_delta_bytes_without_ancestry_rejection() {
    use axum::{Router, routing::post};
    let router = Router::new().route("/v1/responses", post(|bytes: axum::body::Bytes| async move {
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        axum::Json(json!({"status": "completed", "output": [], "echo": body, "usage": {"input_tokens": 100, "output_tokens": 1, "input_tokens_details": {"cached_tokens": 0}}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(temp.path(), address, "off").await;
    let client = reqwest::Client::new();
    for body in [
        json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "old source"}]}),
        json!({"model": "gpt-6-luna", "previous_response_id": "upstream-parent", "input": [{"role": "user", "content": "new delta"}]}),
    ] {
        let response = client
            .post(format!("{url}/v1/responses"))
            .header("x-carry-session", "off-rebase")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "off must never reject native replacements for review ancestry"
        );
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            value["echo"], body,
            "off must preserve parent reference and exact delta"
        );
    }
    server.abort();
}

#[tokio::test]
async fn ephemeral_proxy_listen_announces_its_actual_bound_port() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let state = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_carry"))
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--state-dir",
            state.path().to_str().unwrap(),
        ])
        .env_remove("OPENAI_API_KEY")
        .env_remove("CARRY_PROXY_AUTH_TOKEN")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let banner = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .expect("proxy must announce kernel-assigned port, not a racy reserved port")
        .unwrap()
        .unwrap();
    let address = banner
        .strip_prefix("CARRY_PROXY_LISTEN ")
        .unwrap()
        .parse::<std::net::SocketAddr>()
        .unwrap();
    assert_ne!(address.port(), 0);
    assert!(
        reqwest::get(format!("http://{address}/health"))
            .await
            .unwrap()
            .status()
            .is_success()
    );
}

#[tokio::test]
async fn codex_login_authenticates_primary_and_streamed_reviewer_with_shared_credentials() {
    use axum::{Router, http::HeaderMap, routing::post};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let home = tempfile::tempdir().unwrap();
    let token = format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account"}}"#)
    );
    std::fs::write(
        home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "version": 1, "access_token": token, "refresh_token": "fixture-refresh",
            "expires_at_ms": 4102444800000u64
        }))
        .unwrap(),
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let reviews = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let shadow = reviews.clone();
    let router = Router::new().route("/v1/responses", post(move |headers: HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
        let token = token.clone(); let calls = seen.clone(); let reviews = shadow.clone();
        async move {
            assert_eq!(headers["authorization"], format!("Bearer {token}"));
            assert_eq!(headers["chatgpt-account-id"], "fixture-account");
            assert_eq!(headers["openai-beta"], "responses=experimental");
            assert_eq!(headers["accept"], "text/event-stream");
            assert_eq!(headers["session-id"], body["prompt_cache_key"].as_str().unwrap());
            assert_eq!(body["stream"], true);
            assert_eq!(body["store"], false);
            assert!(body["instructions"].is_string());
            assert!(body.get("max_output_tokens").is_none());
            assert!(body.get("prompt_cache_options").is_none());
            assert!(!body.to_string().contains("prompt_cache_breakpoint"));
            let reviewer = body["prompt_cache_key"] != "fixture-main";
            let text = if reviewer {
                assert_eq!(body["text"]["format"]["type"], "json_schema");
                assert_eq!(body["text"]["format"]["strict"], true);
                reviews.fetch_add(1, Ordering::SeqCst);
                r#"{"protected":[],"removable":[],"memories":[]}"#
            } else { calls.fetch_add(1, Ordering::SeqCst); "fixture answer" };
            let value = json!({"status":"completed", "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],
                "usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":0}}});
            // Like some subscription deployments, deliberately mislabel the SSE.
            ([ ("content-type", "application/json") ], format!("data: {}\n\n", json!({"type":"response.completed","response":value})))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        address,
        "audit",
        &[
            "--codex-login",
            "--codex-home",
            home.path().to_str().unwrap(),
        ],
        "",
    )
    .await;
    let client = reqwest::Client::new();
    let mut input = vec![json!({"role":"user","content":"goal"})];
    for n in 0..3 {
        let response = client
            .post(format!("{url}/v1/responses"))
            .header("x-carry-session", "codex-fixture")
            .json(&json!({"model":"gpt-6-sol", "input":input, "stream":false,
                "max_output_tokens":1000, "prompt_cache_key":"fixture-main",
                "prompt_cache_options":{"mode":"implicit"}}))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(value["status"], "completed");
        input.extend(value["output"].as_array().unwrap().clone());
        input.push(json!({"role":"user","content":format!("followup {n}")}));
    }
    let response = client.post(format!("{url}/v1/responses"))
        .header("x-carry-session", "codex-stream-fixture")
        .json(&json!({"model":"gpt-6-sol", "stream":true, "input":[{"role":"system","content":"Preserve this system instruction"},{"role":"user","content":"stream test"}], "prompt_cache_key":"fixture-main"}))
        .send().await.unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("response.completed")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(
        reviews.load(Ordering::SeqCst) >= 1,
        "OAuth reviewer must consume SSE"
    );
    let metrics: serde_json::Value = client
        .get(format!("{url}/carry/metrics"))
        .header("x-carry-session", "codex-fixture")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metrics["invalid_reviews"], 0);
    assert_eq!(metrics["completed_requests"], 3);
    let dashboard: serde_json::Value = reqwest::get(format!("{url}/carry/dashboard/stats"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(dashboard["reviewer_cache_mode"], "codex-implicit");
    assert_eq!(dashboard["classifier_explicit_cache"], false);
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn standalone_startup_can_launch_browser_without_client_coupling() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let browser = tempfile::tempdir().unwrap();
    let capture = browser.path().join("opened-url");
    for command in ["open", "xdg-open"] {
        let path = browser.path().join(command);
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$CARRY_BROWSER_CAPTURE\"\n",
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (_proxy, url) = start_proxy_options_env(
        state.path(),
        fixture.address,
        "compact",
        &["--open-dashboard"],
        "",
        &[
            ("PATH", browser.path().to_str().unwrap()),
            ("CARRY_BROWSER_CAPTURE", capture.to_str().unwrap()),
        ],
    )
    .await;
    for _ in 0..100 {
        if capture.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        capture.exists(),
        "standalone startup must actually invoke the browser launcher"
    );
    assert_eq!(
        std::fs::read_to_string(capture).unwrap().trim(),
        format!("{url}/carry/dashboard")
    );
    assert!(fixture.requests.lock().await.is_empty());
}

#[tokio::test]
async fn standalone_startup_saves_dashboard_link_without_model_calls() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "compact",
        &[
            "--no-open-dashboard",
            "--classifier-cache-policy",
            "openai-explicit",
        ],
        "standalone-token",
    )
    .await;
    let path = state.path().join("dashboard-url");
    assert!(
        path.exists(),
        "standalone proxy must publish its dashboard without a client launcher"
    );
    let link = std::fs::read_to_string(&path).unwrap();
    assert_eq!(link.trim(), format!("{url}/carry/dashboard"));
    assert!(!link.contains("standalone-token"));
    let response = reqwest::get(link.trim()).await.unwrap();
    assert!(response.status().is_success());
    let stats = reqwest::get(format!("{url}/carry/dashboard/stats"))
        .await
        .unwrap();
    assert!(stats.status().is_success());
    let data: serde_json::Value = stats.json().await.unwrap();
    assert_eq!(data["reviewer_cache_mode"], "openai-explicit");
    assert_eq!(data["classifier_cache_policy"], "openai-explicit");
    assert_eq!(data["classifier_explicit_cache"], true);
    assert!(
        fixture.requests.lock().await.is_empty(),
        "startup must not consume model capacity"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}

#[tokio::test]
async fn codex_login_rejects_non_codex_remote_endpoints_before_loading_credentials() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_carry"))
        .args([
            "proxy",
            "--codex-login",
            "--codex-home",
            home.path().to_str().unwrap(),
            "--upstream-url",
            "https://untrusted.example/responses",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("official Codex endpoint"));
}

#[tokio::test]
async fn codex_primary_fallback_cache_key_is_stable_and_scoped_to_session() {
    use axum::{Router, routing::post};
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::sync::Arc;
    let home = tempfile::tempdir().unwrap();
    let token = format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account"}}"#)
    );
    std::fs::write(
        home.path().join("auth.json"),
        serde_json::to_vec(&json!({
        "version":1, "access_token":token, "refresh_token":"fixture-refresh",
        "expires_at_ms":4102444800000u64}))
        .unwrap(),
    )
    .unwrap();
    let bodies = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let router = Router::new().route("/v1/responses", post(move |axum::Json(body): axum::Json<serde_json::Value>| {
        let captured = captured.clone();
        async move {
            captured.lock().await.push(body);
            ([("content-type", "text/event-stream")], "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":100,\"output_tokens\":1,\"input_tokens_details\":{\"cached_tokens\":50}}}}\n\n")
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        address,
        "off",
        &[
            "--codex-login",
            "--codex-home",
            home.path().to_str().unwrap(),
        ],
        "",
    )
    .await;
    for (session, caller_key) in [
        ("a", None),
        ("a", None),
        ("b", None),
        ("a", Some("caller-owned")),
    ] {
        let mut body = json!({"model":"gpt-6-luna", "input":[{"role":"user","content":"goal"}]});
        if let Some(key) = caller_key {
            body["prompt_cache_key"] = json!(key);
        }
        let response = send(&url, &body, session, "main").await;
        assert!(response.status().is_success());
        response.bytes().await.unwrap();
    }
    let bodies = bodies.lock().await;
    assert_eq!(
        bodies[0]["prompt_cache_key"], bodies[1]["prompt_cache_key"],
        "repeat primary requests in one explicit session must share cache affinity"
    );
    assert_ne!(bodies[0]["prompt_cache_key"], bodies[2]["prompt_cache_key"]);
    assert_eq!(bodies[3]["prompt_cache_key"], "caller-owned");
    server.abort();
}

#[tokio::test]
async fn dashboard_is_public_locally_and_reports_stats_without_conversation_content() {
    use axum::{Router, routing::post};
    let router = Router::new().route(
        "/v1/responses",
        post(|| async {
            axum::Json(json!({"status":"completed", "output":[], "usage":{
            "input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":40}}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) =
        start_proxy_options(state.path(), address, "off", &[], "dashboard-token").await;
    let client = reqwest::Client::new();
    let shell = client
        .get(format!("{url}/carry/dashboard"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        shell.status(),
        200,
        "dashboard shell must be available in a browser"
    );
    assert!(shell.text().await.unwrap().contains("Carry Proxy"));
    let stats_url = format!("{url}/carry/dashboard/stats");
    assert_eq!(client.get(&stats_url).send().await.unwrap().status(), 200);
    for endpoint in ["/v1/models", "/carry/metrics"] {
        assert_eq!(
            client
                .get(format!("{url}{endpoint}"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    assert_eq!(
        client
            .post(format!("{url}/v1/responses"))
            .json(&json!({"model": "gpt-6-luna", "input": []}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for session in ["one", "two"] {
        let response = client.post(format!("{url}/v1/responses"))
            .bearer_auth("dashboard-token").header("x-carry-session",session)
            .json(&json!({"model":"gpt-6-luna", "client_metadata":if session=="one" {json!({"x-codex-turn-metadata":json!({"thread_source":"subagent","agent_name":"/root/worker","parent_thread_id":"PRIVATE_PARENT_DO_NOT_EXPOSE"}).to_string()})}else{json!({"session_kind":"conversation","agent_name":"generic-client"})}, "input":[{"role":"user","content":format!("{} PRIVATE_TASK_DO_NOT_EXPOSE", "Session title ".repeat(10))}]}))
            .send().await.unwrap();
        assert!(response.status().is_success());
        response.bytes().await.unwrap();
    }
    let response = client.get(&stats_url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let data: serde_json::Value = response.json().await.unwrap();
    assert_eq!(data["mode"], "off");
    assert_eq!(data["classifier_model"], "gpt-6-luna");
    assert_eq!(data["reviewer_cache_mode"], "provider-implicit");
    assert_eq!(data["classifier_explicit_cache"], false);
    assert_eq!(data["sessions"].as_array().unwrap().len(), 2);
    let histories = data["sessions"].as_array().unwrap();
    assert_eq!(
        histories
            .iter()
            .filter(|s| s["client"]["kind"] == "agent")
            .count(),
        1
    );
    assert_eq!(
        histories
            .iter()
            .filter(|s| s["client"]["kind"] == "conversation")
            .count(),
        1
    );
    let subagent = histories
        .iter()
        .find(|s| s["client"]["kind"] == "agent")
        .unwrap();
    assert_eq!(subagent["client"]["agent_name"], "/root/worker");
    assert!(!data.to_string().contains("PRIVATE_PARENT_DO_NOT_EXPOSE"));
    for session in data["sessions"].as_array().unwrap() {
        assert!(
            session["title"]
                .as_str()
                .unwrap()
                .starts_with("Session title")
        );
        assert!(session["title"].as_str().unwrap().chars().count() <= 81);
        assert_eq!(session["completed_requests"], 1);
        assert_eq!(session["primary"]["input_tokens"], 100);
        assert_eq!(session["primary"]["cached_tokens"], 40);
        assert_eq!(session["context"]["retained_items"], 1);
    }
    assert!(!data.to_string().contains("PRIVATE_TASK_DO_NOT_EXPOSE"));
    assert!(!data.to_string().contains("dashboard-token"));
    server.abort();
}

async fn start_proxy(
    state: &std::path::Path,
    upstream: std::net::SocketAddr,
    mode: &str,
) -> (Proxy, String) {
    start_proxy_options(state, upstream, mode, &[], "").await
}

async fn start_proxy_options(
    state: &std::path::Path,
    upstream: std::net::SocketAddr,
    mode: &str,
    extra: &[&str],
    gateway: &str,
) -> (Proxy, String) {
    start_proxy_options_env(state, upstream, mode, extra, gateway, &[]).await
}

async fn start_proxy_options_env(
    state: &std::path::Path,
    upstream: std::net::SocketAddr,
    mode: &str,
    extra: &[&str],
    gateway: &str,
    env: &[(&str, &str)],
) -> (Proxy, String) {
    let mut proxy = Proxy(
        Command::new(env!("CARGO_BIN_EXE_carry"))
            .args([
                "proxy",
                "--listen",
                "127.0.0.1:0",
                "--upstream-url",
                &format!("http://{upstream}/v1/responses"),
                "--classifier-model",
                "gpt-6-luna",
                "--min-payback-percent",
                "0",
                "--mode",
                mode,
                "--state-dir",
                state.to_str().unwrap(),
            ])
            .args(if extra.contains(&"--codex-login") {
                vec![]
            } else {
                vec![
                    "--classifier-url".to_owned(),
                    format!("http://{upstream}/review"),
                ]
            })
            // These fixtures exercise synchronous review with a five-request horizon.
            .args(
                if extra
                    .iter()
                    .any(|arg| arg.starts_with("--background-reviews"))
                {
                    vec![]
                } else {
                    vec!["--background-reviews=false"]
                },
            )
            .args(if extra.contains(&"--payoff-requests") {
                vec![]
            } else {
                vec!["--payoff-requests", "5"]
            })
            .args(extra)
            .env_remove("OPENAI_API_KEY")
            .env("CARRY_PROXY_AUTH_TOKEN", gateway)
            .env("CARRY_PROXY_UPSTREAM_KEY", "fixture-primary")
            .env("CARRY_PROXY_CLASSIFIER_KEY", "fixture-shadow")
            .env("CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS", "0")
            .env("CARRY_PROXY_REVIEW_MIN_NEW_TOKENS", "0")
            .env("CARRY_PROXY_REVIEW_BACKOFF_FACTOR", "1")
            .env("CARRY_PROXY_REVIEW_EVERY_REQUESTS", "1")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut reader = std::io::BufReader::new(proxy.0.stdout.take().unwrap());
    let mut banner = String::new();
    std::io::BufRead::read_line(&mut reader, &mut banner).unwrap();
    let address: std::net::SocketAddr = banner
        .trim()
        .strip_prefix("CARRY_PROXY_LISTEN ")
        .expect("proxy must announce its own bound listener before readiness")
        .parse()
        .unwrap();
    proxy.0.stdout = Some(reader.into_inner());
    let url = format!("http://{address}");
    for _ in 0..100 {
        if reqwest::get(format!("{url}/health"))
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return (proxy, url);
        }
        assert!(
            proxy.0.try_wait().unwrap().is_none(),
            "proxy must implement the requested review mode"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("proxy did not become ready");
}

#[tokio::test]
async fn classifier_cache_affinity_is_bounded_stable_and_identity_isolated() {
    use axum::{Router, routing::post};
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let primary = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let shadow = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = primary.clone();
    let reviewed = shadow.clone();
    let router = Router::new()
        .route("/v1/responses", post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let captured = captured.clone();
            async move {
                captured.lock().await.push(body);
                axum::Json(json!({"status": "completed", "output": [],
                    "usage": {"input_tokens": 100, "output_tokens": 1, "input_tokens_details": {"cached_tokens": 0}}}))
            }
        }))
        .route("/review", post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let reviewed = reviewed.clone();
            async move {
                reviewed.lock().await.push(body);
                axum::Json(json!({"status": "completed", "output": [{"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "{\"protected\":[],\"removable\":[],\"memories\":[]}"}]}],
                    "usage": {"input_tokens": 100, "output_tokens": 1, "input_tokens_details": {"cached_tokens": 0}}}))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let owner = "a".repeat(64);
    let other_owner = format!("{}b", "a".repeat(63));
    let mut keys = std::collections::HashSet::new();
    for (tenant, session, branch) in [
        ("tenant-a", owner.as_str(), "left"),
        ("tenant-b", owner.as_str(), "left"),
        ("tenant-a", owner.as_str(), "right"),
        ("tenant-a", other_owner.as_str(), "left"),
    ] {
        let id = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(tenant, session, branch)).unwrap())
        );
        let request = json!({"model": "gpt-6-luna", "prompt_cache_key": id, "input": [
            {"role": "user", "content": "goal"},
            {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "a", "output": "tool result"},
            {"role": "user", "content": "finish"}
        ]});
        // Restart between each generation: affinity must survive checkpoint reload.
        for turn in 0..3 {
            let (_proxy, url) = start_proxy(state.path(), address, "compact").await;
            let response = reqwest::Client::new()
                .post(format!("{url}/v1/responses"))
                .header("x-carry-tenant", tenant)
                .header("x-carry-session", session)
                .header("x-carry-branch", branch)
                .json(&request)
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
            let _ = response.bytes().await.unwrap();
            settled(state.path()).await;
            assert_eq!(
                primary.lock().await.last().unwrap(),
                &request,
                "primary bytes/settings must not change"
            );
            if turn == 0 {
                continue;
            }
            let reviews = shadow.lock().await;
            let review = reviews.last().unwrap();
            assert_eq!(
                review["input"][0],
                json!({"role": "user", "content": "Return JSON."}),
                "JSON-mode instruction must be a stable reviewer-only input message"
            );
            let key = review["prompt_cache_key"].as_str().unwrap();
            assert!(
                key.len() <= 64,
                "classifier cache key exceeds provider's 64-character bound: {}",
                key.len()
            );
            assert_ne!(key, id, "review/main cache domains must remain separate");
            assert!(
                !review["input"].to_string().contains(session),
                "raw session owner is not model prompt data"
            );
            if turn == 1 {
                assert!(
                    keys.insert(key.to_owned()),
                    "tenant/branch/fresh-owner cache collision"
                );
            } else {
                assert_eq!(
                    key,
                    reviews[reviews.len() - 2]["prompt_cache_key"]
                        .as_str()
                        .unwrap()
                );
            }
        }
    }
    assert_eq!(keys.len(), 4);
    assert_eq!(shadow.lock().await.len(), 8);
    server.abort();
}

#[tokio::test]
async fn failed_classifier_records_only_bounded_content_free_provider_fields() {
    use axum::{Router, routing::post};
    use std::sync::Arc;
    use tokio::sync::Mutex;
    let failures = Arc::new(Mutex::new(std::collections::VecDeque::from([
        json!({"error": {"type": "invalid_request_error", "code": "string_above_max_length", "param": "prompt_cache_key",
            "message": "PRIVATE_PROVIDER_MESSAGE fixture-shadow", "request": "PRIVATE_REQUEST_EXCERPT"}, "credentials": "PRIVATE_CREDENTIAL"}),
        json!({"error": {"type": "PRIVATE TYPE WITH SPACES", "code": "x".repeat(65), "param": "PRIVATE\nPARAM",
            "message": "PRIVATE_PROVIDER_MESSAGE"}}),
        json!({"error": {"type": 42, "code": null, "param": []}}),
    ])));
    let router = Router::new()
        .route("/v1/responses", post(|| async { axum::Json(json!({"status": "completed", "output": [],
            "usage": {"input_tokens": 100, "output_tokens": 1, "input_tokens_details": {"cached_tokens": 0}}})) }))
        .route("/review", post(move || {
            let failures = failures.clone();
            async move { (axum::http::StatusCode::BAD_REQUEST, axum::Json(failures.lock().await.pop_front().unwrap())) }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) =
        start_proxy_options(state.path(), address, "compact", &["--verbose"], "").await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "goal"},
        {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "a", "output": "exact retained source"},
        {"role": "user", "content": "finish"}
    ]});
    for _ in 0..4 {
        let response = send(&url, &request, "a", "main").await;
        assert!(response.status().is_success());
        let _ = response.bytes().await.unwrap();
        settled(state.path()).await;
    }
    let saved = settled(state.path()).await.remove(0);
    assert_eq!(saved["invalid_reviews"], 3);
    assert_eq!(saved["shadow"]["unavailable_cost_calls"], 3);
    assert_eq!(saved["compactions"], 0);
    let trace_file = std::fs::read_dir(state.path())
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .unwrap();
    let trace = std::fs::read_to_string(trace_file.path()).unwrap();
    assert!(!trace.contains("PRIVATE_"));
    assert!(!trace.contains("fixture-shadow"));
    let failed = trace
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|e| e["event"] == "shadow_failed")
        .map(|e| e["data"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        failed.len(),
        3,
        "each HTTP rejection needs content-free diagnostics without a paid replay"
    );
    assert_eq!(
        failed[0],
        json!({"reason": "review_transport_or_protocol_failure", "http_status": 400,
        "error_type": "invalid_request_error", "error_code": "string_above_max_length", "error_param": "prompt_cache_key"})
    );
    for failure in &failed[1..] {
        assert_eq!(
            failure,
            &json!({"reason": "review_transport_or_protocol_failure", "http_status": 400})
        );
    }
    server.abort();
}

#[tokio::test]
async fn compact_removes_atomic_cohort_from_primary_and_active_shadow() {
    use axum::{Router, routing::post};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let primary = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let shadow = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = primary.clone();
    let reviewed = shadow.clone();
    let native_captured = primary.clone();
    let router = Router::new()
        .route(
            "/v1/responses",
            post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let captured = captured.clone();
                async move {
                    captured.lock().await.push(body);
                    axum::Json(json!({
                        "id": "resp_fixture", "status": "completed",
                        "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]}],
                        "usage": {"input_tokens": 20000, "output_tokens": 1, "input_tokens_details": {"cached_tokens": 0}}
                    }))
                }
            }),
        )
        .route(
            "/v1/responses/compact",
            post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let captured = native_captured.clone();
                async move {
                    captured.lock().await.push(body);
                    axum::Json(json!({
                        "output": [{"type": "compaction", "id": "cmp_1", "encrypted_content": "NATIVE_CHECKPOINT"}],
                        "usage": {"input_tokens": 50, "output_tokens": 8, "input_tokens_details": {"cached_tokens": 0}}
                    }))
                }
            }),
        )
        .route(
            "/review",
            post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let reviewed = reviewed.clone();
                async move {
                    reviewed.lock().await.push(body);
                    let advice = json!({
                        "protected": ["g1"], "removable": ["g2"],
                        "memories": [
                            {"source_ids": ["g2"], "text": "both tools succeeded"},
                            {"source_ids": ["g2"], "text": "Migration unfinished on feat/migration; artifacts/migration.diff; next run fixture B; schema decision unresolved."}
                        ]
                    });
                    axum::Json(json!({
                        "status": "completed",
                        "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": advice.to_string()}]}],
                        "usage": {"input_tokens": 20000, "output_tokens": 9000, "input_tokens_details": {"cached_tokens": 0}}
                    }))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    // This fixture tests exposure earned by a live primary, not replay import.
    let (_proxy, url) = start_proxy_options(
        temp.path(),
        address,
        "compact",
        &["--bootstrap-min-input-bytes", "8388608"],
        "",
    )
    .await;
    let old = "DISCARD_SOURCE_PAYLOAD_".repeat(2000);
    let input = json!([
        {"role": "user", "content": "preserve requirement"},
        {"type": "reasoning", "id": "rs_old", "summary": [], "encrypted_content": "opaque-reasoning"},
        {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
        {"type": "function_call", "call_id": "b", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "a", "output": old},
        {"type": "function_call_output", "call_id": "b", "output": "Migration unfinished on feat/migration; artifacts/migration.diff; next run fixture B; schema decision unresolved."},
        {"role": "user", "content": "finish original task"}
    ]);
    let client = reqwest::Client::new();
    let request = json!({"model": "gpt-6-luna", "input": input, "store": false});
    let first: serde_json::Value = client
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "fixture-session")
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(primary.lock().await[0], request);
    assert!(
        shadow.lock().await.is_empty(),
        "unexposed groups cannot be reviewed"
    );
    let mut second = request.clone();
    second["input"]
        .as_array_mut()
        .unwrap()
        .extend(first["output"].as_array().unwrap().iter().cloned());
    second["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "user", "content": "continue"}));
    let result = client
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "fixture-session")
        .json(&second)
        .send()
        .await
        .unwrap();
    assert!(result.status().is_success());
    let _ = result.bytes().await.unwrap();
    settled(temp.path()).await;
    assert_eq!(shadow.lock().await.len(), 1);
    assert!(
        primary.lock().await[1]
            .to_string()
            .contains("DISCARD_SOURCE_PAYLOAD_")
    );
    // Restart the real proxy: accepted outcome/WIP memories and source opinions
    // must survive durable loading before the following request selects a rewrite.
    drop(_proxy);
    let (_restarted, url) = start_proxy_options(
        temp.path(),
        address,
        "compact",
        &["--bootstrap-min-input-bytes", "8388608"],
        "",
    )
    .await;
    let third = client
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "fixture-session")
        .json(&second)
        .send()
        .await
        .unwrap();
    assert!(third.status().is_success());
    third.bytes().await.unwrap();
    let sent = primary.lock().await[2].clone();
    assert!(!sent.to_string().contains("DISCARD_SOURCE_PAYLOAD_"));
    assert!(!sent.to_string().contains("opaque-reasoning"));
    assert!(sent.to_string().contains("both tools succeeded"));
    assert_eq!(
        sent["input"][1],
        json!({"role":"user","content":"[Earlier conversation omitted by Carry; retained messages remain in original order.]"})
    );
    assert!(sent.to_string().contains("preserve requirement"));
    assert_eq!(sent["input"].as_array().unwrap().len(), 7);
    let fact = &sent["input"][3]["content"];
    let fact: serde_json::Value =
        serde_json::from_str(fact.as_str().unwrap().split_once(": ").unwrap().1).unwrap();
    assert_eq!(
        fact["text"],
        "Migration unfinished on feat/migration; artifacts/migration.diff; next run fixture B; schema decision unresolved."
    );
    let states = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(entry.path()).unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(states.len(), 1);
    let active = states[0]["active_shadow"].to_string();
    assert!(!active.contains("DISCARD_SOURCE_PAYLOAD_"));
    assert!(!active.contains("opaque-reasoning"));
    assert!(
        states[0]["active_shadow"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| {
                let data: serde_json::Value =
                    serde_json::from_str(record["value"]["content"].as_str().unwrap()).unwrap();
                data["group_id"] != "g2"
            }),
        "removed observation/opinion must not survive as actionable group data"
    );
    assert!(active.contains("preserve requirement"));
    assert!(active.contains("both tools succeeded"));
    assert!(active.contains("schema decision unresolved"));
    let cost_stats: serde_json::Value = reqwest::get(format!("{url}/carry/dashboard/stats"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let estimate = &cost_stats["sessions"][0]["cost_estimate"];
    assert!(estimate.get("priced").is_none());
    assert!(estimate["carry_cost"].is_number());
    assert_eq!(estimate["coverage"]["primary_responses"], 3);
    assert_eq!(estimate["coverage"]["reviewer_responses"], 1);
    assert!(estimate["reviewer_cost"].as_f64().unwrap() > 0.0);
    assert_eq!(estimate["traditional_compactions"], 0);
    let carry = estimate["carry_cost"].as_f64().unwrap();
    let traditional = estimate["traditional_cost"].as_f64().unwrap();
    assert!((estimate["savings"].as_f64().unwrap() - (traditional - carry)).abs() < 1e-12);
    assert_eq!(cost_stats["sessions"][0]["title"], "preserve requirement");
    let mut diagnostics = cost_stats.clone();
    diagnostics["sessions"][0]
        .as_object_mut()
        .unwrap()
        .remove("title");
    assert!(!diagnostics.to_string().contains("preserve requirement"));
    assert!(!diagnostics.to_string().contains("both tools succeeded"));
    let checkpoint: serde_json::Value = client
        .post(format!("{url}/v1/responses/compact"))
        .header("x-carry-session", "fixture-session")
        .json(&second)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        checkpoint["output"][0]["encrypted_content"],
        "NATIVE_CHECKPOINT"
    );
    assert_eq!(
        primary.lock().await[3]["input"],
        sent["input"],
        "repeated projection must preserve notice/fact positions"
    );
    assert!(
        !primary.lock().await[3]
            .to_string()
            .contains("DISCARD_SOURCE_PAYLOAD_"),
        "native compaction must operate on the same retained main view, not resurrect removed echo source"
    );
    let stale = client
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "fixture-session")
        .json(&second)
        .send()
        .await
        .unwrap();
    assert_eq!(
        stale.status(),
        reqwest::StatusCode::CONFLICT,
        "after native compaction an old uncheckpointed echo cannot resurrect removed main source"
    );
    let resumed = json!({"model": "gpt-6-luna", "input": [checkpoint["output"][0].clone(), json!({"role": "user", "content": "next goal"})], "store": false});
    let response = client
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "fixture-session")
        .json(&resumed)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let _ = response.bytes().await.unwrap();
    assert_eq!(primary.lock().await[4], resumed);
    server.abort();
}

struct Fixture {
    address: std::net::SocketAddr,
    requests: std::sync::Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
    reviews: std::sync::Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const COMPLETED_SSE: &str = "event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"café\"}\r\n\r\nevent: response.completed\r\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_sse\",\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"id\":\"fc_native\",\"call_id\":\"sse_call\",\"name\":\"native\",\"arguments\":\"{}\",\"status\":\"completed\"}],\"usage\":{\"input_tokens\":11,\"output_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":0}}}}\r\n\r\ndata: [DONE]\r\n\r\n";
const FAILED_SSE: &str = "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"usage\":{\"input_tokens\":22,\"output_tokens\":1,\"input_tokens_details\":{\"cached_tokens\":0}}}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n";

async fn fixture(advice: serde_json::Value) -> Fixture {
    use axum::{
        Router,
        body::{Body, Bytes},
        response::IntoResponse,
        routing::{get, post},
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = requests.clone();
    let reviews = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let reviewed = reviews.clone();
    let router = Router::new()
        .route("/v1/models", get(|| async { axum::Json(json!({"object": "list", "data": [{"id": "gpt-6-luna"}]})) }))
        .route("/v1/responses", post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let captured = captured.clone();
            async move {
                captured.lock().await.push(body.clone());
                match body["metadata"]["scenario"].as_str() {
                    Some("completed_sse" | "failed_sse") => {
                        let bytes = if body["metadata"]["scenario"] == "completed_sse" { COMPLETED_SSE.as_bytes() } else { FAILED_SSE.as_bytes() };
                        let chunks = bytes.chunks(3).map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk))).collect::<Vec<_>>();
                        ([ ("content-type", "text/event-stream"), ("x-fixture-native", "unchanged") ], Body::from_stream(tokio_stream::iter(chunks))).into_response()
                    }
                    Some("completed_sse_open") => {
                        let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
                        tokio::spawn(async move {
                            let _ = sender.send(Ok(Bytes::from_static(COMPLETED_SSE.as_bytes()))).await;
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            drop(sender);
                        });
                        ([("content-type", "text/event-stream")], Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver))).into_response()
                    }
                    Some("cancellable_sse") => {
                        let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
                        tokio::spawn(async move {
                            let _ = sender.send(Ok(Bytes::from_static(b"data: {\"type\":\"response.created\"}\n\n"))).await;
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            let _ = sender.send(Ok(Bytes::from_static(COMPLETED_SSE.as_bytes()))).await;
                        });
                        ([("content-type", "text/event-stream")], Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver))).into_response()
                    }
                    Some("http_error") => (axum::http::StatusCode::TOO_MANY_REQUESTS, "native-quota-body").into_response(),
                    Some("delayed") => {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        axum::Json(json!({"status": "completed", "output": []})).into_response()
                    }
                    _ => axum::Json(json!({
                        "id": "resp_json", "status": "completed",
                        "output": [{"type": "message", "id": "msg_native", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "done", "annotations": [], "logprobs": []}]}],
                        "usage": {"input_tokens": 11, "output_tokens": 2, "input_tokens_details": {"cached_tokens": 0}}
                    })).into_response(),
                }
            }
        }))
        .route("/review", post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let advice = advice.clone();
            let reviewed = reviewed.clone();
            async move {
                reviewed.lock().await.push(body);
                if advice["fixture_delay"] == true {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                axum::Json(json!({
                "status": "completed", "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": advice.to_string()}]}],
                "usage": {"input_tokens": 7, "output_tokens": 2, "input_tokens_details": {"cached_tokens": 0}}
            })) }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Fixture {
        address,
        requests,
        reviews,
        task,
    }
}

fn checkpoint_states(path: &std::path::Path) -> Vec<serde_json::Value> {
    // Advice becomes authority at the atomic index publication, not at its
    // earlier per-session diagnostic mirror write. Explicit legacy sessions
    // remain mirror-owned; implicit selected snapshots are index-owned.
    let index = std::fs::read(path.join("history.index"))
        .ok()
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).unwrap());
    std::fs::read_dir(path)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let mirror: serde_json::Value =
                serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap();
            if mirror["history_shared"] == true {
                let name = e.path().file_stem()?.to_str()?.to_owned();
                index.as_ref()?.get("snapshots")?.get(name).cloned()
            } else {
                Some(mirror)
            }
        })
        .collect()
}

// Legacy advice/receipt assertions run only after the primary body is read.
// This is fixture synchronization, not primary admission or a rescue review.
async fn settled(path: &std::path::Path) -> Vec<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let states = checkpoint_states(path);
            if states
                .iter()
                .all(|s| s["background_review_pending"] != true)
            {
                break states;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("post-response reviewer must settle")
}

async fn send(
    url: &str,
    request: &serde_json::Value,
    tenant: &str,
    branch: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .header("x-carry-tenant", tenant)
        .header("x-carry-session", "session")
        .header("x-carry-branch", branch)
        .json(request)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn explicit_replay_reviews_after_first_primary_without_exposing_fresh_tool_results() {
    for mode in ["compact", "audit", "off"] {
        let fixture =
            fixture(json!({"protected": ["g1"], "removable": ["g2"], "memories": []})).await;
        let state = tempfile::tempdir().unwrap();
        let (proxy, url) = start_proxy_options(
            state.path(),
            fixture.address,
            mode,
            &["--review-every-requests", "5", "--background-reviews"],
            "",
        )
        .await;
        let request = json!({"model": "gpt-6-luna", "input": [
            {"role": "user", "content": "original requirement"},
            {"type": "function_call", "call_id": "old", "name": "native", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "old", "output": "REPLAY_OLD_SOURCE".repeat(3000)},
            {"role": "user", "content": "current request"},
            {"type": "function_call", "call_id": "fresh", "name": "native", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "fresh", "output": "FRESH_RESULT_MUST_REACH_PRIMARY"}
        ]});
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .header("x-carry-session", "replayed")
            .header("x-carry-replay-history", "before-latest-user")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.bytes().await.unwrap();
        let saved = settled(state.path()).await.remove(0);
        assert_eq!(
            saved["shadow"]["calls"],
            if mode == "off" { 0 } else { 1 },
            "bootstrap completes independently after request one, regardless of ordinary cadence"
        );
        assert_eq!(saved["invalid_reviews"], 0);
        let primary = fixture.requests.lock().await[0].clone();
        assert!(primary.to_string().contains("REPLAY_OLD_SOURCE"));
        assert!(
            primary
                .to_string()
                .contains("FRESH_RESULT_MUST_REACH_PRIMARY")
        );
        assert!(primary.to_string().contains("current request"));
        assert!(primary.to_string().contains("original requirement"));
        assert_eq!(
            saved["completed_requests"], 1,
            "imported history is not a completed proxy request"
        );
        drop(proxy);
        let (_proxy, url) = start_proxy_options(
            state.path(),
            fixture.address,
            mode,
            &["--review-replayed-history", "--review-every-requests", "5"],
            "",
        )
        .await;
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .header("x-carry-session", "replayed")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.bytes().await.unwrap();
        let resumed = fixture.requests.lock().await[1].clone();
        assert_eq!(
            resumed.to_string().contains("REPLAY_OLD_SOURCE"),
            mode != "compact",
            "accepted advice becomes actionable only on the later primary"
        );
        assert!(
            resumed
                .to_string()
                .contains("FRESH_RESULT_MUST_REACH_PRIMARY")
        );
        assert!(resumed.to_string().contains("original requirement"));
        assert!(resumed.to_string().contains("current request"));
        let saved = settled(state.path()).await.remove(0);
        assert_eq!(
            saved["shadow"]["calls"],
            if mode == "off" { 0 } else { 1 },
            "a recovered lineage uses normal cadence, not another bootstrap import"
        );
        assert_eq!(saved["completed_requests"], 2);
    }
}

#[tokio::test]
async fn replay_opt_in_does_not_review_a_fresh_system_and_user_request() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "system", "content": "initial instruction"},
        {"role": "user", "content": "first user request"}
    ]});
    let response = reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "fresh")
        .header("x-carry-replay-history", "before-latest-user")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    response.bytes().await.unwrap();
    assert_eq!(
        checkpoint_states(state.path()).remove(0)["shadow"]["calls"],
        0,
        "a fresh initial instruction is not a replayed conversation turn"
    );
    assert_eq!(fixture.requests.lock().await[0], request);
}

#[tokio::test]
async fn replay_import_does_not_reclassify_fresh_suffix_on_failed_primary_retry() {
    let fixture = fixture(json!({"protected": ["g1"], "removable": ["g2"], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "old goal"},
        {"type": "function_call", "call_id": "old", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "old", "output": "old source".repeat(3000)},
        {"role": "user", "content": "new goal"},
        {"type": "function_call", "call_id": "fresh", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "fresh", "output": "new result"}
    ], "metadata": {"scenario": "http_error"}});
    for attempt in 0..2 {
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .header("x-carry-session", "replayed")
            .header("x-carry-replay-history", "before-latest-user")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        response.bytes().await.unwrap();
        let saved = settled(state.path()).await.remove(0);
        for i in 0..6 {
            assert_eq!(
                saved["history"][i]["exposed"],
                i < 3,
                "only caller-declared replay prefix is previously consumed history"
            );
        }
        assert_eq!(saved["completed_requests"], 0);
        assert_eq!(saved["invalid_reviews"], 0);
        assert_eq!(
            fixture
                .requests
                .lock()
                .await
                .last()
                .unwrap()
                .to_string()
                .contains("old source"),
            attempt == 0,
            "failed primary retry may use independently accepted bootstrap advice"
        );
    }
}

#[tokio::test]
async fn failed_rebase_primary_reuses_successful_bootstrap_before_review_cadence() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (mut proxy, mut url) = start_proxy(state.path(), fixture.address, "compact").await;
    let seed =
        json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "initial goal"}]});
    send(&url, &seed, "a", "main").await.bytes().await.unwrap();
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "replacement prior goal"},
        {"role": "assistant", "content": "prior evidence"},
        {"role": "user", "content": "replacement latest goal"}
    ], "metadata": {"scenario": "http_error"}});
    for attempt in 1..=2 {
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .header("x-carry-session", "session")
            .header("x-carry-tenant", "a")
            .header("x-carry-history-policy", "reset-on-divergence")
            .header("x-carry-replay-history", "before-latest-user")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        response.bytes().await.unwrap();
        let saved = settled(state.path()).await.remove(0);
        assert_eq!(saved["completed_requests"], 1);
        assert_eq!(saved["history_rebases"], 1);
        assert_eq!(saved["invalid_reviews"], 0);
        assert_eq!(
            saved["shadow"]["calls"], 1,
            "successful bootstrap chunks survive failed primaries and restarts without replay"
        );
        assert_eq!(saved["history"][2]["exposed"], false);
        assert_eq!(saved["bootstrap_review_pending"], false);
        assert_eq!(saved["bootstrap_review_complete"], true);
        if attempt == 1 {
            drop(proxy);
            let restarted = start_proxy(state.path(), fixture.address, "compact").await;
            proxy = restarted.0;
            url = restarted.1;
        }
    }
}

#[tokio::test]
async fn fragmented_sse_preserves_bytes_and_exposes_only_completed_primary_input() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "off").await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "call native"}], "stream": true, "metadata": {"scenario": "completed_sse"}});
    let response = send(&url, &request, "a", "main").await;
    assert_eq!(response.headers()["x-fixture-native"], "unchanged");
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        COMPLETED_SSE.as_bytes()
    );
    let saved = settled(state.path()).await.remove(0);
    assert_eq!(saved["history"].as_array().unwrap().len(), 1);
    assert_eq!(saved["history"][0]["exposed"], true);
    assert_eq!(saved["pending_output"][0]["call_id"], "sse_call");
    let failed = json!({"model": "gpt-6-luna", "input": [
        request["input"][0].clone(),
        {"type": "function_call", "call_id": "sse_call", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "sse_call", "output": "fresh native result"}
    ], "stream": true, "metadata": {"scenario": "failed_sse"}});
    assert_eq!(
        send(&url, &failed, "a", "main")
            .await
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        FAILED_SSE.as_bytes()
    );
    let saved = settled(state.path()).await.remove(0);
    assert_eq!(saved["history"][1]["exposed"], false);
    assert_eq!(saved["history"][2]["exposed"], false);
    assert_eq!(saved["completed_requests"], 1);
    assert_eq!(saved["failed_primaries"], 1);
    assert_eq!(
        saved["primary"]["input_tokens"], 33,
        "failed native usage remains billable even though it grants no exposure"
    );
}

#[tokio::test]
async fn branch_tenant_and_restart_are_explicit_and_credentials_are_not_persisted() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "initial"}], "store": false});
    let _ = send(&url, &request, "a", "left")
        .await
        .bytes()
        .await
        .unwrap();
    drop(proxy);
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let metrics: serde_json::Value = reqwest::Client::new()
        .get(format!("{url}/carry/metrics"))
        .header("x-carry-session", "session")
        .header("x-carry-tenant", "a")
        .header("x-carry-branch", "left")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metrics["completed_requests"], 1);
    let _ = send(&url, &request, "a", "right")
        .await
        .bytes()
        .await
        .unwrap();
    let _ = send(&url, &request, "b", "left")
        .await
        .bytes()
        .await
        .unwrap();
    assert_eq!(checkpoint_states(state.path()).len(), 3);
    let diverged = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "different ancestry"}]});
    assert_eq!(
        send(&url, &diverged, "a", "left").await.status(),
        reqwest::StatusCode::CONFLICT
    );
    let models: serde_json::Value = reqwest::get(format!("{url}/v1/models"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"][0]["id"], "gpt-6-luna");
    fn assert_private_state(directory: &std::path::Path) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(entry.metadata().unwrap().permissions().mode() & 0o077, 0);
            }
            if entry.file_type().unwrap().is_dir() {
                assert_private_state(&entry.path());
            } else {
                let data = std::fs::read(entry.path()).unwrap();
                let text = String::from_utf8_lossy(&data);
                assert!(!text.contains("fixture-primary"));
                assert!(!text.contains("fixture-shadow"));
            }
        }
    }
    // Cover every persisted file, including the nested operational journal.
    assert_private_state(state.path());
}

#[tokio::test]
async fn background_review_does_not_block_primary_or_duplicate_inflight_work() {
    use axum::{Router, response::IntoResponse, routing::post};
    use std::sync::Arc;
    for scenario in [
        "continuation",
        "rebase",
        "failed_primary",
        "restart",
        "reviewer_failure",
    ] {
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());
        let reviews = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let (gate, signal, captured) = (release.clone(), started.clone(), reviews.clone());
        let router = Router::new()
        .route("/v1/responses", post(|axum::Json(body): axum::Json<serde_json::Value>| async move {
            if body["metadata"]["fail"] == true {
                return (axum::http::StatusCode::TOO_MANY_REQUESTS,"failed primary").into_response();
            }
            axum::Json(json!({"status":"completed","output":[],"usage":{
                "input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}})).into_response()
        }))
        .route("/review", post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let (gate, signal, captured) = (gate.clone(), signal.clone(), captured.clone());
            async move {
                captured.lock().await.push(body);
                signal.notify_one();
                gate.notified().await;
                if scenario == "reviewer_failure" {
                    return (axum::http::StatusCode::SERVICE_UNAVAILABLE,"review unavailable").into_response();
                }
                let advice = json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"background fact"}]});
                axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}],"usage":{
                    "input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}})).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let state = tempfile::tempdir().unwrap();
        let (proxy, url) = start_proxy_options(
            state.path(),
            address,
            "audit",
            &[
                "--background-reviews",
                "--bootstrap-min-input-bytes",
                "8388608",
            ],
            "",
        )
        .await;
        let mut request = json!({"model":"gpt-6-luna","input":[
        {"role":"user","content":"goal"},{"role":"assistant","content":"old evidence"},{"role":"user","content":"current"}]});
        send(&url, &request, "async", "main")
            .await
            .bytes()
            .await
            .unwrap();
        let primary = tokio::spawn({
            let (url, request) = (url.clone(), request.clone());
            async move {
                send(&url, &request, "async", "main")
                    .await
                    .bytes()
                    .await
                    .unwrap()
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), primary)
            .await
            .expect("primary must complete while the reviewer is still blocked")
            .unwrap();
        let stats: serde_json::Value = reqwest::get(format!("{url}/carry/dashboard/stats"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(stats["sessions"][0]["background_review_pending"], true);
        let estimate = &stats["sessions"][0]["cost_estimate"];
        assert!(estimate.get("priced").is_none());
        for field in [
            "carry_cost",
            "traditional_cost",
            "reviewer_cost",
            "summary_cost",
            "savings",
            "savings_percent",
        ] {
            assert!(
                estimate[field].is_null(),
                "pending reviewer spend cannot be reported as {field}"
            );
        }
        assert!(stats["cost_estimate"]["savings"].is_null());
        assert_eq!(stats["cost_estimate"]["excluded_sessions"], 1);
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"simultaneous continuation"}));
        if scenario == "failed_primary" {
            request["metadata"] = json!({"fail":true});
        }
        if scenario == "rebase" {
            request["input"] = json!([{ "role":"user", "content":"replacement task" }]);
            reqwest::Client::new()
                .post(format!("{url}/v1/responses"))
                .header("x-carry-tenant", "async")
                .header("x-carry-session", "session")
                .header("x-carry-branch", "main")
                .header("x-carry-history-policy", "reset-on-divergence")
                .json(&request)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
        } else if scenario == "restart" {
            drop(proxy);
            let (_restarted, restarted_url) = start_proxy_options(
                state.path(),
                address,
                "audit",
                &[
                    "--background-reviews",
                    "--bootstrap-min-input-bytes",
                    "8388608",
                    "--review-every-requests",
                    "100",
                ],
                "",
            )
            .await;
            send(&restarted_url, &request, "async", "main")
                .await
                .bytes()
                .await
                .unwrap();
            let saved = settled(state.path()).await.remove(0);
            assert_eq!(saved["completed_requests"], 3);
            assert_eq!(saved["shadow"]["calls"], 1);
            assert_eq!(saved["shadow"]["unavailable_cost_calls"], 1);
            assert_eq!(saved["background_review_pending"], false);
            assert_eq!(saved["memories"], json!([]));
            server.abort();
            continue;
        } else {
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                send(&url, &request, "async", "main"),
            )
            .await
            .unwrap();
            if scenario == "failed_primary" {
                assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
            }
            response.bytes().await.unwrap();
        }
        assert_eq!(
            reviews.lock().await.len(),
            1,
            "one review in flight per session"
        );
        release.notify_one();
        let saved = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let saved = settled(state.path()).await.remove(0);
                if saved["shadow"]["calls"] == 1 {
                    break saved;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            saved["completed_requests"],
            if scenario == "failed_primary" { 2 } else { 3 },
            "merge must preserve newer primary counters"
        );
        assert_eq!(
            saved["history"].as_array().unwrap().len(),
            if scenario == "rebase" { 1 } else { 4 },
            "merge must preserve the newer continuation"
        );
        if scenario == "rebase" {
            assert_eq!(saved["opinions"], json!({}));
            assert_eq!(saved["memories"], json!([]));
            assert!(
                saved["review_feedback"]["rejected"][0]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("discarded")
            );
        } else if scenario == "reviewer_failure" {
            assert_eq!(saved["opinions"], json!({}));
            assert_eq!(saved["memories"], json!([]));
            assert_eq!(saved["shadow"]["unavailable_cost_calls"], 1);
            assert_eq!(saved["invalid_reviews"], 1);
            assert_eq!(saved["background_review_pending"], false);
        } else {
            assert_eq!(saved["opinions"]["2"], "drop");
            assert_eq!(saved["memories"][0]["text"], "background fact");
        }
        assert_eq!(
            saved["cost_trajectory"]["result"]["coverage"]["reviewer_responses"],
            if scenario == "reviewer_failure" { 0 } else { 1 }
        );
        server.abort();
    }
}

#[tokio::test]
async fn mixed_advice_applies_valid_items_and_failed_primary_keeps_feedback() {
    let fixture = fixture(json!({"protected": ["g2"], "removable": ["g2"], "memories": [{"source_ids": ["g2"], "text": "valid sourced fact"}]})).await;
    let state = tempfile::tempdir().unwrap();
    // Preserve the live-primary exposure boundary rather than importing replay.
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "compact",
        &["--bootstrap-min-input-bytes", "8388608"],
        "",
    )
    .await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "requirement"},
        {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "a", "output": "UNIQUE_TOOL_SOURCE".repeat(2000)},
        {"role": "user", "content": "finish"}
    ], "store": false});
    let first: serde_json::Value = send(&url, &request, "a", "main")
        .await
        .json()
        .await
        .unwrap();
    let mut next = request.clone();
    next["input"]
        .as_array_mut()
        .unwrap()
        .extend(first["output"].as_array().unwrap().iter().cloned());
    next["metadata"] = json!({"scenario": "http_error"});
    let response = send(&url, &next, "a", "main").await;
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.text().await.unwrap(), "native-quota-body");
    let saved = settled(state.path()).await.remove(0);
    assert_eq!(saved["invalid_reviews"], 0);
    assert_eq!(saved["partial_reviews"], 1);
    assert_eq!(saved["shadow"]["calls"], 1);
    assert_eq!(saved["memories"][0]["text"], "valid sourced fact");
    assert_eq!(saved["opinions"]["2"], "keep");
    assert_eq!(saved["review_feedback"]["rejected_count"], 1);
    assert_eq!(
        saved["review_feedback"]["rejected"][0]["field"],
        "removable"
    );
    assert!(
        saved["review_feedback"]["rejected"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("conflicting")
    );
    assert_eq!(saved["compactions"], 0);
    assert!(
        fixture.requests.lock().await[1]
            .to_string()
            .contains("UNIQUE_TOOL_SOURCE")
    );
    assert_eq!(saved["history"][4]["exposed"], false);
}

#[tokio::test]
async fn unknown_stateful_ancestry_and_unsupported_retrieval_fail_explicitly() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "audit").await;
    let plain = json!({"model": "gpt-6-luna", "input": []});
    let missing = reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .json(&json!({"model": "gpt-6-luna", "input": [], "previous_response_id": "resp_opaque"}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::CONFLICT);
    for field in ["conversation", "background"] {
        let mut request = plain.clone();
        request[field] = if field == "background" {
            json!(true)
        } else {
            json!("opaque")
        };
        assert_eq!(
            send(&url, &request, "a", "main").await.status(),
            if field == "background" {
                reqwest::StatusCode::BAD_REQUEST
            } else {
                reqwest::StatusCode::CONFLICT
            }
        );
    }
    assert_eq!(
        reqwest::get(format!("{url}/v1/responses/resp_opaque"))
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_IMPLEMENTED
    );
    assert!(fixture.requests.lock().await.is_empty());
}

#[tokio::test]
async fn client_close_after_terminal_completion_commits_exposure_without_http_eof() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "off").await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "goal"}],
        "stream": true, "metadata": {"scenario": "completed_sse_open"}});
    let mut response = send(&url, &request, "a", "main").await;
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !String::from_utf8_lossy(&received).contains("event: response.completed") {
            received.extend_from_slice(&response.chunk().await.unwrap().unwrap());
        }
    })
    .await
    .expect("terminal event must arrive while upstream is still open");
    assert_eq!(received, COMPLETED_SSE.as_bytes());
    drop(response);
    let mut saved = checkpoint_states(state.path()).remove(0);
    for _ in 0..100 {
        if saved["completed_requests"] == 1 || saved["failed_primaries"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        saved = checkpoint_states(state.path()).remove(0);
    }
    assert_eq!(
        saved["completed_requests"], 1,
        "native completion survives a post-terminal client close"
    );
    assert_eq!(saved["failed_primaries"], 0);
    assert_eq!(saved["history"][0]["exposed"], true);
}

#[tokio::test]
async fn primary_timeout_and_client_cancellation_are_censored_without_exposure() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "off",
        &["--request-timeout-secs", "1"],
        "",
    )
    .await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "goal"}], "metadata": {"scenario": "delayed"}});
    assert_eq!(
        send(&url, &request, "a", "main").await.status(),
        reqwest::StatusCode::BAD_GATEWAY
    );
    let saved = settled(state.path()).await.remove(0);
    assert_eq!(saved["history"][0]["exposed"], false);
    assert_eq!(
        saved["primary"]["calls"], 1,
        "a timed-out attempt remains in the fixed denominator"
    );
    assert_eq!(saved["primary"]["unavailable_cost_calls"], 1);
    let mut cancelled = request;
    cancelled["metadata"]["scenario"] = json!("cancellable_sse");
    let mut response = send(&url, &cancelled, "a", "main").await;
    assert!(response.chunk().await.unwrap().is_some());
    drop(response);
    let mut saved = checkpoint_states(state.path()).remove(0);
    for _ in 0..100 {
        if saved["failed_primaries"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        saved = checkpoint_states(state.path()).remove(0);
    }
    assert_eq!(saved["failed_primaries"], 2);
    assert_eq!(saved["primary"]["calls"], 2);
    assert_eq!(saved["completed_requests"], 0);
    assert_eq!(saved["history"][0]["exposed"], false);
}

#[tokio::test]
async fn gateway_token_is_separate_and_not_an_upstream_credential() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) =
        start_proxy_options(state.path(), fixture.address, "off", &[], "fixture-gateway").await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "goal"}]});
    assert_eq!(
        send(&url, &request, "a", "main").await.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let response = reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .bearer_auth("fixture-gateway")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let _ = response.bytes().await.unwrap();
    assert_eq!(fixture.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn valid_completed_shadow_and_shared_memory_survive_a_failed_primary() {
    let fixture = fixture(json!({"protected": ["g1"], "removable": ["g2"], "memories": [{"source_ids": ["g2"], "text": "remember tested outcome"}]})).await;
    let state = tempfile::tempdir().unwrap();
    // Preserve the live-primary exposure boundary rather than importing replay.
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "compact",
        &["--bootstrap-min-input-bytes", "8388608"],
        "",
    )
    .await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "goal"},
        {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "a", "output": "large".repeat(10000)},
        {"role": "user", "content": "finish"}
    ], "store": false});
    let first: serde_json::Value = send(&url, &request, "a", "main")
        .await
        .json()
        .await
        .unwrap();
    let mut next = request;
    next["input"]
        .as_array_mut()
        .unwrap()
        .extend(first["output"].as_array().unwrap().iter().cloned());
    next["metadata"] = json!({"scenario": "http_error"});
    let _ = send(&url, &next, "a", "main").await.bytes().await.unwrap();
    let saved = settled(state.path()).await.remove(0);
    assert_eq!(saved["invalid_reviews"], 0);
    assert_eq!(saved["shadow"]["calls"], 1);
    assert_eq!(saved["memories"][0]["text"], "remember tested outcome");
    assert_eq!(saved["opinions"]["2"], "drop");
    assert_eq!(
        saved["compactions"], 0,
        "failed primary does not commit a speculative removal"
    );
    assert_eq!(
        saved["cost_trajectory"]["result"]["coverage"]["failed_calls"],
        1
    );
    assert_eq!(
        saved["cost_trajectory"]["restored"], 0,
        "failed rewrites must not restore hypothetical context"
    );
    assert_eq!(saved["history"][1]["removed"], false);
    assert_eq!(saved["history"][4]["exposed"], false);
}

#[tokio::test]
async fn reviewer_timeout_fails_closed_while_primary_can_complete() {
    let fixture = fixture(json!({"fixture_delay": true})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "compact",
        &["--classifier-timeout-secs", "1"],
        "",
    )
    .await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "goal"},
        {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "a", "output": "preserve exact source"},
        {"role": "user", "content": "finish"}
    ], "store": false});
    let first: serde_json::Value = send(&url, &request, "a", "main")
        .await
        .json()
        .await
        .unwrap();
    let mut next = request;
    next["input"]
        .as_array_mut()
        .unwrap()
        .extend(first["output"].as_array().unwrap().iter().cloned());
    let second = send(&url, &next, "a", "main").await;
    assert!(second.status().is_success());
    // Primary usage/checkpoint commit completes while the response is consumed;
    // an HTTP status alone can precede the second successful-request debit.
    second.bytes().await.unwrap();
    let saved = settled(state.path()).await.remove(0);
    assert_eq!(saved["completed_requests"], 2);
    assert_eq!(saved["shadow"]["calls"], 1);
    assert_eq!(saved["shadow"]["unavailable_cost_calls"], 1);
    assert_eq!(saved["invalid_reviews"], 1);
    assert_eq!(fixture.requests.lock().await[1], next);
}

#[tokio::test]
async fn native_codex_and_pi_echo_shapes_preserve_settings_and_unknown_model_passthrough() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "compact",
        &["--primary-context-tokens", "272000"],
        "",
    )
    .await;
    for (branch, model) in [("codex", "gpt-6-luna"), ("pi", "unknown-provider-model")] {
        let request = json!({
            "model": model, "input": [{"role": "user", "content": [{"type": "input_text", "text": "use native tools"}]}],
            "instructions": "native client instructions", "tools": [{"type": "function", "name": "native", "parameters": {"type": "object", "properties": {}}}],
            "reasoning": {"effort": "medium", "summary": "auto"}, "parallel_tool_calls": true,
            "text": {"verbosity": "low"}, "include": ["reasoning.encrypted_content"], "store": false,
            "prompt_cache_key": format!("native-affinity-{branch}")
        });
        let first: serde_json::Value = send(&url, &request, "a", branch)
            .await
            .json()
            .await
            .unwrap();
        let mut next = request.clone();
        let mut output = first["output"][0].clone();
        if branch == "pi" {
            let object = output.as_object_mut().unwrap();
            object.remove("id");
            object.remove("status");
            object.remove("type");
            output["content"][0]
                .as_object_mut()
                .unwrap()
                .remove("annotations");
            output["content"][0]
                .as_object_mut()
                .unwrap()
                .remove("logprobs");
        }
        next["input"].as_array_mut().unwrap().push(output);
        next["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "user", "content": [{"type": "input_text", "text": "continue"}]}));
        let _ = send(&url, &next, "a", branch).await.bytes().await.unwrap();
        let captures = fixture.requests.lock().await;
        assert_eq!(captures[captures.len() - 2], request);
        assert_eq!(captures[captures.len() - 1], next);
    }
    let pi_state = checkpoint_states(state.path())
        .into_iter()
        .find(|s| s["review_context"]["model"] == "unknown-provider-model")
        .unwrap();
    assert_eq!(pi_state["last_plan"]["reason"], "unsupported_model_or_tier");
    assert_eq!(pi_state["primary"]["unavailable_cost_calls"], 2);
}

#[tokio::test]
async fn numeric_benchmark_events_match_attempts_usage_and_censored_failures() {
    use std::io::Read;
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (mut proxy, url) = start_proxy(state.path(), fixture.address, "off").await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "PRIVATE_SOURCE_NOT_PUBLIC_TELEMETRY"}]});
    let _ = send(&url, &request, "a", "main")
        .await
        .bytes()
        .await
        .unwrap();
    let mut failed = request;
    failed["metadata"] = json!({"scenario": "http_error"});
    let _ = send(&url, &failed, "a", "main")
        .await
        .bytes()
        .await
        .unwrap();
    proxy.0.kill().unwrap();
    proxy.0.wait().unwrap();
    let mut output = String::new();
    proxy
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(!output.contains("PRIVATE_SOURCE_NOT_PUBLIC_TELEMETRY"));
    assert!(!output.contains("fixture-primary"));
    let events = output
        .lines()
        .filter_map(|line| line.strip_prefix("BENCHMARK_CONTEXT_EVENT "))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let starts = events
        .iter()
        .filter(|e| e["actor"] == "primary" && e["event"] == "started")
        .collect::<Vec<_>>();
    assert_eq!(
        starts.len(),
        2,
        "each real attempt has numeric telemetry before network I/O"
    );
    assert_ne!(starts[0]["request_id"], starts[1]["request_id"]);
    let completed = events
        .iter()
        .filter(|e| e["actor"] == "primary" && e["event"] == "completed")
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["usage"]["input_tokens"], 11);
    assert_eq!(completed[0]["usage"]["output_tokens"], 2);
    assert_eq!(completed[0]["model"], "gpt-6-luna");
    assert!(completed[0]["latency_ms"].is_number());
}

#[tokio::test]
async fn client_continuation_is_expanded_but_never_forwarded_upstream() {
    use axum::{Router, routing::post};
    use std::sync::Arc;
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let seen = requests.clone();
    let router = Router::new().route("/v1/responses", post(move |axum::Json(body): axum::Json<serde_json::Value>| {
        let seen = seen.clone();
        async move {
            let mut requests = seen.lock().await;
            requests.push(body);
            axum::Json(json!({"id": format!("resp_{}", requests.len()), "status": "completed", "output": [{"role": "assistant", "content": "answer"}]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let (proxy, url) = start_proxy_options(
        state.path(),
        address,
        "compact",
        &["--primary-context-tokens", "272000"],
        "",
    )
    .await;
    let first = json!({"model": "fixture-model", "input": [{"role": "user", "content": "goal"}]});
    let response = send(&url, &first, "a", "main").await;
    assert!(response.status().is_success());
    let _: serde_json::Value = response.json().await.unwrap();
    let delta = json!({"model": "fixture-model", "previous_response_id": "resp_1", "input": [{"role": "user", "content": "next"}]});
    let response = send(&url, &delta, "a", "main").await;
    assert!(
        response.status().is_success(),
        "known client continuation must be accepted"
    );
    let _: serde_json::Value = response.json().await.unwrap();
    let captured = requests.lock().await;
    assert_eq!(captured.len(), 2);
    assert!(captured[1].get("previous_response_id").is_none());
    assert_eq!(
        captured[1]["input"],
        json!([{"role": "user", "content": "goal"}, {"role": "assistant", "content": "answer"}, {"role": "user", "content": "next"}])
    );
    drop(captured);
    assert_eq!(
        send(&url, &delta, "other", "main").await.status(),
        reqwest::StatusCode::CONFLICT
    );
    let rewind = send(&url, &delta, "a", "main").await;
    assert!(
        rewind.status().is_success(),
        "an observed historical response ID is a valid rewind"
    );
    let _: serde_json::Value = rewind.json().await.unwrap();
    {
        let captured = requests.lock().await;
        assert_eq!(captured[2]["input"], captured[1]["input"]);
    }
    drop(proxy);
    let (_proxy, url) = start_proxy_options(
        state.path(),
        address,
        "compact",
        &["--primary-context-tokens", "272000"],
        "",
    )
    .await;
    let resumed = json!({"model": "fixture-model", "previous_response_id": "resp_2", "input": [{"role": "user", "content": "after restart"}]});
    let response = send(&url, &resumed, "a", "main").await;
    assert!(response.status().is_success());
    let _: serde_json::Value = response.json().await.unwrap();
    let captured = requests.lock().await;
    assert_eq!(captured.len(), 4);
    assert!(captured[3].get("previous_response_id").is_none());
    assert_eq!(captured[3]["input"].as_array().unwrap().len(), 5);
    assert_eq!(captured[3]["input"][4]["content"], "after restart");
    server.abort();
}

#[tokio::test]
async fn implicit_history_reuses_removals_and_keeps_divergent_history_separate() {
    let fixture = fixture(json!({"protected": ["g1"], "removable": ["g2"], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let original = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "old requirement"},
        {"type": "function_call", "call_id": "old", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "old", "output": "IMPLICIT_OLD_SOURCE".repeat(3000)},
        {"role": "user", "content": "latest task"}
    ]});
    async fn implicit(url: &str, body: &serde_json::Value) {
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .json(body)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "full histories need no explicit session header"
        );
        response.bytes().await.unwrap();
    }
    implicit(&url, &original).await;
    settled(state.path()).await;
    assert!(
        fixture.requests.lock().await[0]
            .to_string()
            .contains("IMPLICIT_OLD_SOURCE")
    );
    drop(proxy);
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let mut extended = original.clone();
    extended["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "user", "content": "continue"}));
    implicit(&url, &extended).await;
    assert!(
        !fixture.requests.lock().await[1]
            .to_string()
            .contains("IMPLICIT_OLD_SOURCE")
    );
    let replacement =
        json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "different history"}]});
    implicit(&url, &replacement).await;
    assert_eq!(fixture.requests.lock().await[2], replacement);
    let saved = checkpoint_states(state.path());
    assert_eq!(
        saved.len(),
        2,
        "unrelated history has its own selection, not a destructive reset"
    );
    let fresh = saved
        .iter()
        .find(|s| s["history"].as_array().unwrap().len() == 1)
        .unwrap();
    assert_eq!(fresh["history_rebases"], 0);
    assert!(fresh["memories"].as_array().unwrap().is_empty());
    assert!(fresh["opinions"].as_object().unwrap().is_empty());
}

#[tokio::test]
async fn unseen_explicit_history_bootstraps_but_small_histories_skip_replay() {
    for (size, calls) in [(10, 0), (30_000, 1)] {
        let fixture = fixture(json!({"protected": [], "removable": ["g2"], "memories": []})).await;
        let state = tempfile::tempdir().unwrap();
        let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
        let request = json!({"model": "gpt-6-luna", "input": [
            {"role": "user", "content": "old goal"},
            {"role": "assistant", "content": "X".repeat(size)},
            {"role": "user", "content": "current goal"}
        ]});
        let response = send(&url, &request, "a", "main").await;
        assert!(response.status().is_success());
        response.bytes().await.unwrap();
        settled(state.path()).await;
        assert_eq!(fixture.reviews.lock().await.len(), calls);
        assert_eq!(
            fixture.requests.lock().await[0]["input"]
                .as_array()
                .unwrap()
                .len(),
            3,
            "bootstrap does not rewrite the first primary"
        );
    }
}

#[tokio::test]
async fn oversized_replay_groups_are_kept_whole_without_reviewer_dispatch() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "compact",
        &["--classifier-max-input-bytes", "4096"],
        "",
    )
    .await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "old goal"},
        {"type": "function_call", "call_id": "large", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "large", "output": "X".repeat(30_000)},
        {"role": "user", "content": "new goal"}
    ]});
    let response = reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .header("x-carry-session", "oversized")
        .header("x-carry-replay-history", "before-latest-user")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.bytes().await.unwrap();
    settled(state.path()).await;
    assert_eq!(fixture.requests.lock().await[0], request);
    let reviews = fixture.reviews.lock().await;
    assert!(
        reviews
            .iter()
            .all(|body| serde_json::to_vec(body).unwrap().len() <= 4096)
    );
    assert!(
        reviews
            .iter()
            .all(|body| !body.to_string().contains("function_call")),
        "never review a partial oversized tool group"
    );
    let saved = settled(state.path()).await.remove(0);
    assert!(
        saved["history"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["removed"] == false)
    );
    assert_eq!(saved["bootstrap_review_pending"], true);
}

#[tokio::test]
async fn unseen_history_with_only_pinned_sources_skips_bootstrap_calls() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": [{"type": "input_text", "text": "goal"}, {"type": "input_image", "image_url": "opaque"}]},
        {"type": "unknown_native", "opaque": "X".repeat(30_000)},
        {"role": "user", "content": "current goal"}
    ]});
    let response = send(&url, &request, "a", "main").await;
    assert!(response.status().is_success());
    response.bytes().await.unwrap();
    assert!(
        fixture.reviews.lock().await.is_empty(),
        "nothing is safely removable"
    );
    assert_eq!(fixture.requests.lock().await[0], request);
    let saved = settled(state.path()).await.remove(0);
    assert_eq!(saved["bootstrap_review_pending"], false);
}

#[tokio::test]
async fn implicit_prefix_state_is_isolated_by_tenant_branch_and_settings() {
    let fixture = fixture(json!({"protected": [], "removable": ["g2"], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let original = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "old goal"},
        {"role": "assistant", "content": "OLD_EVIDENCE".repeat(3000)},
        {"role": "user", "content": "current goal"}
    ]});
    for (tenant, branch, instructions) in [
        ("a", "main", ""),
        ("b", "main", ""),
        ("a", "other", ""),
        ("a", "main", "new settings"),
    ] {
        let mut request = original.clone();
        if !instructions.is_empty() {
            request["instructions"] = json!(instructions);
        }
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .header("x-carry-tenant", tenant)
            .header("x-carry-branch", branch)
            .json(&request)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.bytes().await.unwrap();
        settled(state.path()).await;
    }
    assert_eq!(checkpoint_states(state.path()).len(), 4);
    assert_eq!(
        fixture.reviews.lock().await.len(),
        4,
        "no cross-owner/settings retention reuse"
    );
    assert!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .all(|body| body.to_string().contains("OLD_EVIDENCE")),
        "first requests cannot consume advice not yet accepted"
    );
}

#[tokio::test]
async fn implicit_unseen_history_can_replace_a_native_checkpoint_epoch() {
    use axum::{Router, routing::post};
    let router = Router::new().route("/v1/responses", post(|axum::Json(body): axum::Json<serde_json::Value>| async move {
        let native = body["input"].as_array().unwrap().iter().any(|item| item["type"] == "compaction_trigger");
        axum::Json(json!({"status": "completed", "output": if native { vec![json!({"type": "compaction", "encrypted_content": "opaque"})] } else { vec![] }}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), address, "compact").await;
    for request in [
        json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "goal"}, {"type": "compaction_trigger"}]}),
        json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "unrelated fresh history"}]}),
    ] {
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .json(&request)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "implicit histories replace, rather than assert, ancestry"
        );
        response.bytes().await.unwrap();
    }
    let saved = settled(state.path()).await;
    assert_eq!(
        saved.len(),
        2,
        "native epoch and unrelated history stay separate"
    );
    assert!(saved.iter().all(|s| s["history_rebases"] == 0));
    assert!(saved.iter().any(|s| s["native_checkpoint"] == json!([])));
    server.abort();
}

#[tokio::test]
async fn client_turn_metadata_preserves_proxy_session_and_reviewer_affinity() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "audit").await;
    let mut request = json!({"model":"gpt-6-luna","input":[
        {"role":"user","content":"goal"},
        {"role":"assistant","content":"evidence ".repeat(3000)},
        {"role":"user","content":"current"}
    ]});
    for turn in ["turn-one", "turn-two"] {
        request["client_metadata"] = json!({"session_id":"session","thread_id":"thread","turn_id":turn,"root_turn_id":turn,
            "x-codex-turn-metadata":json!({"session_id":"session","thread_id":"thread","turn_id":turn,"root_turn_id":turn,"turn_started_at_unix_ms":turn,"turn_trigger":turn,"source":turn,"client_type":turn}).to_string()});
        let response: serde_json::Value = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        settled(state.path()).await;
        request["input"]
            .as_array_mut()
            .unwrap()
            .extend(response["output"].as_array().unwrap().iter().cloned());
    }
    let saved = checkpoint_states(state.path());
    assert_eq!(
        saved.len(),
        1,
        "turn metadata must not fork durable session state"
    );
    assert_eq!(saved[0]["completed_requests"], 2);
    request["client_metadata"]["thread_id"] = json!("another-thread");
    reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .json(&request)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    settled(state.path()).await;
    assert_eq!(
        checkpoint_states(state.path()).len(),
        2,
        "threads remain isolated"
    );
    let reviews = fixture.reviews.lock().await;
    assert!(reviews.len() >= 2);
    assert_eq!(
        reviews[0]["prompt_cache_key"],
        reviews[1]["prompt_cache_key"]
    );
    let forwarded = fixture.requests.lock().await;
    assert_eq!(forwarded[0]["client_metadata"]["turn_id"], "turn-one");
    assert_eq!(forwarded[1]["client_metadata"]["turn_id"], "turn-two");
}

#[tokio::test]
async fn reviewer_transport_preserves_encrypted_reasoning_and_native_tool_pairs() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "audit",
        &["--classifier-cache-policy", "openai-explicit"],
        "",
    )
    .await;
    let reasoning = json!({"type":"reasoning","id":"rs_fixture","encrypted_content":"ENCRYPTED_NATIVE_FIXTURE","summary":[]});
    let call = json!({"type":"function_call","id":"fc_fixture","call_id":"native_pair","name":"inspect","arguments":"{}"});
    let result = json!({"type":"function_call_output","call_id":"native_pair","output":"native evidence ".repeat(3000)});
    let request = json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"goal"},reasoning.clone(),call.clone(),result.clone(),{"role":"user","content":"current"}]});
    let response = reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    response.bytes().await.unwrap();
    settled(state.path()).await;
    let reviews = fixture.reviews.lock().await;
    assert_eq!(reviews[0]["model"], "gpt-6-luna");
    let input = reviews[0]["input"].as_array().unwrap();
    assert!(input.contains(&reasoning));
    assert!(input.contains(&call));
    assert!(input.contains(&result));
    let forwarded = fixture.requests.lock().await;
    assert_eq!(forwarded[0]["input"], request["input"]);
}

#[tokio::test]
async fn unknown_primary_pricing_skips_reviews_that_cannot_produce_a_plan() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        fixture.address,
        "compact",
        &[
            "--review-replayed-history",
            "--primary-context-tokens",
            "272000",
        ],
        "",
    )
    .await;
    let request = json!({"model":"unpriced-model", "input":[
        {"role":"user","content":"previous task"},
        {"role":"assistant","content":"historical output ".repeat(2000)},
        {"role":"user","content":"current request"}
    ]});
    reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .json(&request)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        fixture.reviews.lock().await.len(),
        0,
        "unknown primary pricing cannot repay any reviewer spend"
    );
    assert_eq!(fixture.requests.lock().await[0]["input"], request["input"]);
    let saved = checkpoint_states(state.path());
    assert_eq!(saved[0]["primary"]["calls"], 1);
    assert_eq!(saved[0]["primary"]["unavailable_cost_calls"], 1);
    assert_eq!(saved[0]["compactions"], 0);
}

#[tokio::test]
async fn subscription_model_discovery_authenticates_filters_and_fails_closed() {
    use axum::{
        Router,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let home = tempfile::tempdir().unwrap();
    let token = format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account"}}"#)
    );
    std::fs::write(
        home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "version": 1, "access_token": token, "refresh_token": "fixture-refresh",
            "expires_at_ms": 4102444800000u64
        }))
        .unwrap(),
    )
    .unwrap();
    let scenario = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let calls = requests.clone();
    let mode = scenario.clone();
    let expected_token = token.clone();
    let router = Router::new().route("/v1/models", get(move |headers: HeaderMap, uri: axum::http::Uri| {
        let token = expected_token.clone(); let calls = calls.clone(); let mode = mode.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(headers["authorization"], format!("Bearer {token}"));
            assert_eq!(headers["chatgpt-account-id"], "fixture-account");
            assert_eq!(headers["accept"], "application/json");
            assert_eq!(uri.query(), Some("client_version=0.147.0"));
            match mode.load(Ordering::SeqCst) {
                1 => (StatusCode::SERVICE_UNAVAILABLE, "private upstream diagnostic").into_response(),
                2 => axum::Json(json!({"models": [{"visibility":"list"}]})).into_response(),
                3 => (StatusCode::FOUND, [("location", "/must-not-follow")], "private redirect").into_response(),
                4 => axum::Json(json!({"models": []})).into_response(),
                5 => (StatusCode::UNAUTHORIZED, "private authentication diagnostic").into_response(),
                6 => "x".repeat(8 * 1024 * 1024 + 1).into_response(),
                _ => axum::Json(json!({"models": [
                    {"slug":"fixture-subscription", "visibility":"list", "supported_in_api":false,
                     "base_instructions":"private provider instructions"},
                    {"slug":"hidden", "visibility":"hide"},
                    {"slug":"unlisted", "visibility":"none"}
                ]})).into_response(),
            }
        }
    })).route("/must-not-follow", get(|| async {
        axum::Json(json!({"models":[{"slug":"redirected", "visibility":"list"}]}))
    })).route("/v1/responses", post(move |headers: HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
        let token = token.clone();
        async move {
            assert_eq!(headers["authorization"], format!("Bearer {token}"));
            assert_eq!(headers["chatgpt-account-id"], "fixture-account");
            assert_eq!(body["model"], "fixture-subscription");
            ([ ("content-type", "text/event-stream") ], format!("data: {}\n\n", json!({
                "type":"response.completed", "response":{"id":"fixture-answer", "status":"completed", "output":[]}
            })))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    // The helper also sets an API key: explicit subscription mode must never use it.
    let (_proxy, url) = start_proxy_options(
        state.path(),
        address,
        "off",
        &[
            "--codex-login",
            "--codex-home",
            home.path().to_str().unwrap(),
        ],
        "local-client",
    )
    .await;
    let client = reqwest::Client::new();
    for key in [None, Some("wrong")] {
        let mut request = client.get(format!("{url}/v1/models"));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    let response = client
        .get(format!("{url}/v1/models"))
        .bearer_auth("local-client")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        catalog,
        json!({"object":"list", "data":[{
            "id":"fixture-subscription", "object":"model", "created":0, "owned_by":"openai"
        }]})
    );
    // Select the discovered model through the same OpenAI-compatible HTTP surface.
    let response = client
        .post(format!("{url}/v1/responses"))
        .bearer_auth("local-client")
        .json(&json!({"model":catalog["data"][0]["id"], "input":[], "stream":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["id"],
        "fixture-answer"
    );
    for failure_mode in [1, 2, 3, 5, 6] {
        scenario.store(failure_mode, Ordering::SeqCst);
        let response = client
            .get(format!("{url}/v1/models"))
            .bearer_auth("local-client")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response.text().await.unwrap();
        assert!(!body.contains("private"));
        assert!(!body.contains("fixture-refresh"));
    }
    scenario.store(4, Ordering::SeqCst);
    let catalog: serde_json::Value = client
        .get(format!("{url}/v1/models"))
        .bearer_auth("local-client")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(catalog, json!({"object":"list", "data":[]}));
    server.abort();
    let response = client
        .get(format!("{url}/v1/models"))
        .bearer_auth("local-client")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn api_key_model_discovery_remains_an_authenticated_upstream_relay() {
    use axum::{Router, http::HeaderMap, routing::get};
    let router = Router::new().route(
        "/v1/models",
        get(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer fixture-primary");
            assert!(!headers.contains_key("chatgpt-account-id"));
            axum::Json(json!({"object":"list", "data":[{"id":"fixture-api", "custom":true}]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(
        state.path(),
        address,
        "off",
        &["--login", "api-key"],
        "local-client",
    )
    .await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{url}/v1/models"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let catalog: serde_json::Value = client
        .get(format!("{url}/v1/models"))
        .bearer_auth("local-client")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        catalog,
        json!({"object":"list", "data":[{"id":"fixture-api", "custom":true}]})
    );
    server.abort();
}
