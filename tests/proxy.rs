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
            .env_remove("OPENAI_API_KEY")
            .env_remove("CARRY_PROXY_AUTH_TOKEN")
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
    server.abort();
}
