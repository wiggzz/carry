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
    assert!(healthy, "carry proxy must expose a working HTTP health route");
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
