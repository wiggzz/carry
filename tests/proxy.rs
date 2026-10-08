use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Proxy(Child);

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn proxy_cli_serves_health_and_forwards_opaque_json() {
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let state = tempfile::tempdir().unwrap();
    let mut proxy = Proxy(
        Command::new(env!("CARGO_BIN_EXE_carry"))
            .args([
                "proxy",
                "--listen",
                &address.to_string(),
                "--upstream-url",
                &format!("http://{upstream_address}/v1/responses"),
                "--state-dir",
                state.path().to_str().unwrap(),
            ])
            .env_remove("OPENAI_API_KEY")
            .env_remove("CARRY_PROXY_AUTH_TOKEN")
            .env("CARRY_PROXY_UPSTREAM_KEY", "fixture-only")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
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
                    assert!(headers.contains("Bearer fixture-only"));
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
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let mut proxy = Proxy(
        Command::new(env!("CARGO_BIN_EXE_carry"))
            .args([
                "proxy",
                "--listen",
                &address.to_string(),
                "--upstream-url",
                &format!("http://{upstream}/v1/responses"),
                "--classifier-url",
                &format!("http://{upstream}/review"),
                "--classifier-model",
                "gpt-6-luna",
                "--min-payback-percent",
                "0",
                "--mode",
                mode,
                "--state-dir",
                state.to_str().unwrap(),
            ])
            .args(extra)
            .env_remove("OPENAI_API_KEY")
            .env("CARRY_PROXY_AUTH_TOKEN", gateway)
            .env("CARRY_PROXY_UPSTREAM_KEY", "fixture-primary")
            .env("CARRY_PROXY_CLASSIFIER_KEY", "fixture-shadow")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
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
                        "memories": [{"source_ids": ["g2"], "text": "both tools succeeded"}]
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
    let (_proxy, url) = start_proxy(temp.path(), address, "compact").await;
    let old = "DISCARD_SOURCE_PAYLOAD_".repeat(2000);
    let input = json!([
        {"role": "user", "content": "preserve requirement"},
        {"type": "reasoning", "id": "rs_old", "summary": [], "encrypted_content": "opaque-reasoning"},
        {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
        {"type": "function_call", "call_id": "b", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "a", "output": old},
        {"type": "function_call_output", "call_id": "b", "output": "other output"},
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
    assert_eq!(shadow.lock().await.len(), 1);
    let sent = primary.lock().await[1].clone();
    assert!(!sent.to_string().contains("DISCARD_SOURCE_PAYLOAD_"));
    assert!(!sent.to_string().contains("opaque-reasoning"));
    assert!(sent.to_string().contains("both tools succeeded"));
    assert!(sent.to_string().contains("preserve requirement"));
    assert_eq!(sent["input"].as_array().unwrap().len(), 5);
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
        !active.contains("g2"),
        "mixed review records must be projected, not kept whole"
    );
    assert!(active.contains("preserve requirement"));
    assert!(active.contains("both tools succeeded"));
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
    assert!(
        !primary.lock().await[2]
            .to_string()
            .contains("DISCARD_SOURCE_PAYLOAD_"),
        "native compaction must operate on the same retained main view, not resurrect removed echo source"
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
    assert_eq!(primary.lock().await[3], resumed);
    server.abort();
}

struct Fixture {
    address: std::net::SocketAddr,
    requests: std::sync::Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
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
        .route("/review", post(move || {
            let advice = advice.clone();
            async move {
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
        task,
    }
}

fn checkpoint_states(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_dir(path)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect()
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
    let saved = checkpoint_states(state.path()).remove(0);
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
    let saved = checkpoint_states(state.path()).remove(0);
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
    let (proxy, url) = start_proxy(state.path(), fixture.address, "off").await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "initial"}], "store": false});
    let _ = send(&url, &request, "a", "left")
        .await
        .bytes()
        .await
        .unwrap();
    drop(proxy);
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "off").await;
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
    for entry in std::fs::read_dir(state.path())
        .unwrap()
        .filter_map(Result::ok)
    {
        let data = std::fs::read(entry.path()).unwrap();
        let text = String::from_utf8_lossy(&data);
        assert!(!text.contains("fixture-primary"));
        assert!(!text.contains("fixture-shadow"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(entry.metadata().unwrap().permissions().mode() & 0o077, 0);
        }
    }
}

#[tokio::test]
async fn invalid_mixed_advice_is_atomic_and_failed_primary_keeps_completed_shadow() {
    let fixture = fixture(json!({"protected": ["g2"], "removable": ["g2"], "memories": [{"source_ids": ["g2"], "text": "must not be partially applied"}]})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
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
    let saved = checkpoint_states(state.path()).remove(0);
    assert_eq!(saved["invalid_reviews"], 1);
    assert_eq!(saved["shadow"]["calls"], 1);
    assert_eq!(saved["memories"], json!([]));
    assert_eq!(saved["opinions"], json!({}));
    assert_eq!(saved["compactions"], 0);
    assert!(
        fixture.requests.lock().await[1]
            .to_string()
            .contains("UNIQUE_TOOL_SOURCE")
    );
    assert_eq!(saved["history"][4]["exposed"], false);
}

#[tokio::test]
async fn unsupported_stateful_review_and_retrieval_fail_explicitly() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "audit").await;
    let plain = json!({"model": "gpt-6-luna", "input": []});
    let missing = reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .json(&plain)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::BAD_REQUEST);
    for field in ["previous_response_id", "conversation", "background"] {
        let mut request = plain.clone();
        request[field] = if field == "background" {
            json!(true)
        } else {
            json!("opaque")
        };
        assert_eq!(
            send(&url, &request, "a", "main").await.status(),
            reqwest::StatusCode::BAD_REQUEST
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
async fn primary_timeout_and_client_cancellation_are_censored_without_exposure() {
    let fixture = fixture(json!({"protected": [], "removable": [], "memories": []})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(state.path(), fixture.address, "off", &["--request-timeout-secs", "1"], "").await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "goal"}], "metadata": {"scenario": "delayed"}});
    assert_eq!(send(&url, &request, "a", "main").await.status(), reqwest::StatusCode::BAD_GATEWAY);
    let saved = checkpoint_states(state.path()).remove(0);
    assert_eq!(saved["history"][0]["exposed"], false);
    assert_eq!(saved["primary"]["calls"], 1, "a timed-out attempt remains in the fixed denominator");
    assert_eq!(saved["primary"]["unavailable_cost_calls"], 1);
    let mut cancelled = request;
    cancelled["metadata"]["scenario"] = json!("cancellable_sse");
    let mut response = send(&url, &cancelled, "a", "main").await;
    assert!(response.chunk().await.unwrap().is_some());
    drop(response);
    let mut saved = checkpoint_states(state.path()).remove(0);
    for _ in 0..100 {
        if saved["failed_primaries"] == 2 { break; }
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
    let (_proxy, url) = start_proxy_options(state.path(), fixture.address, "off", &[], "fixture-gateway").await;
    let request = json!({"model": "gpt-6-luna", "input": [{"role": "user", "content": "goal"}]});
    assert_eq!(send(&url, &request, "a", "main").await.status(), reqwest::StatusCode::UNAUTHORIZED);
    let response = reqwest::Client::new().post(format!("{url}/v1/responses"))
        .bearer_auth("fixture-gateway").json(&request).send().await.unwrap();
    assert!(response.status().is_success());
    let _ = response.bytes().await.unwrap();
    assert_eq!(fixture.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn valid_completed_shadow_and_shared_memory_survive_a_failed_primary() {
    let fixture = fixture(json!({"protected": ["g1"], "removable": ["g2"], "memories": [{"source_ids": ["g2"], "text": "remember tested outcome"}]})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "goal"},
        {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "a", "output": "large".repeat(10000)},
        {"role": "user", "content": "finish"}
    ], "store": false});
    let first: serde_json::Value = send(&url, &request, "a", "main").await.json().await.unwrap();
    let mut next = request;
    next["input"].as_array_mut().unwrap().extend(first["output"].as_array().unwrap().iter().cloned());
    next["metadata"] = json!({"scenario": "http_error"});
    let _ = send(&url, &next, "a", "main").await.bytes().await.unwrap();
    let saved = checkpoint_states(state.path()).remove(0);
    assert_eq!(saved["invalid_reviews"], 0);
    assert_eq!(saved["shadow"]["calls"], 1);
    assert_eq!(saved["memories"][0]["text"], "remember tested outcome");
    assert_eq!(saved["opinions"]["2"], "drop");
    assert_eq!(saved["compactions"], 0, "failed primary does not commit a speculative removal");
    assert_eq!(saved["history"][1]["removed"], false);
    assert_eq!(saved["history"][4]["exposed"], false);
}

#[tokio::test]
async fn reviewer_timeout_fails_closed_while_primary_can_complete() {
    let fixture = fixture(json!({"fixture_delay": true})).await;
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options(state.path(), fixture.address, "compact", &["--classifier-timeout-secs", "1"], "").await;
    let request = json!({"model": "gpt-6-luna", "input": [
        {"role": "user", "content": "goal"},
        {"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "a", "output": "preserve exact source"},
        {"role": "user", "content": "finish"}
    ], "store": false});
    let first: serde_json::Value = send(&url, &request, "a", "main").await.json().await.unwrap();
    let mut next = request;
    next["input"].as_array_mut().unwrap().extend(first["output"].as_array().unwrap().iter().cloned());
    assert!(send(&url, &next, "a", "main").await.status().is_success());
    let mut saved = checkpoint_states(state.path()).remove(0);
    for _ in 0..50 {
        if saved["completed_requests"] == 2 { break; }
        tokio::time::sleep(Duration::from_millis(10)).await;
        saved = checkpoint_states(state.path()).remove(0);
    }
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
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
    for (branch, model) in [("codex", "gpt-6-luna"), ("pi", "unknown-provider-model")] {
        let request = json!({
            "model": model, "input": [{"role": "user", "content": [{"type": "input_text", "text": "use native tools"}]}],
            "instructions": "native client instructions", "tools": [{"type": "function", "name": "native", "parameters": {"type": "object", "properties": {}}}],
            "reasoning": {"effort": "medium", "summary": "auto"}, "parallel_tool_calls": true,
            "text": {"verbosity": "low"}, "include": ["reasoning.encrypted_content"], "store": false,
            "prompt_cache_key": format!("native-affinity-{branch}")
        });
        let first: serde_json::Value = send(&url, &request, "a", branch).await.json().await.unwrap();
        let mut next = request.clone();
        let mut output = first["output"][0].clone();
        if branch == "pi" {
            let object = output.as_object_mut().unwrap();
            object.remove("id"); object.remove("status"); object.remove("type");
            output["content"][0].as_object_mut().unwrap().remove("annotations");
            output["content"][0].as_object_mut().unwrap().remove("logprobs");
        }
        next["input"].as_array_mut().unwrap().push(output);
        next["input"].as_array_mut().unwrap().push(json!({"role": "user", "content": [{"type": "input_text", "text": "continue"}]}));
        let _ = send(&url, &next, "a", branch).await.bytes().await.unwrap();
        let captures = fixture.requests.lock().await;
        assert_eq!(captures[captures.len()-2], request);
        assert_eq!(captures[captures.len()-1], next);
    }
    let pi_state = checkpoint_states(state.path()).into_iter().find(|s| s["review_context"]["model"] == "unknown-provider-model").unwrap();
    assert_eq!(pi_state["last_plan"]["reason"], "unsupported_model_or_tier");
    assert_eq!(pi_state["primary"]["unavailable_cost_calls"], 2);
}
