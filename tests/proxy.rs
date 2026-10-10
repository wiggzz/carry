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
            .json(&json!({"model":"gpt-6-luna", "input":[{"role":"user","content":"PRIVATE_TASK_DO_NOT_EXPOSE"}]}))
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
    for session in data["sessions"].as_array().unwrap() {
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
            .args(extra)
            .env_remove("OPENAI_API_KEY")
            .env("CARRY_PROXY_AUTH_TOKEN", gateway)
            .env("CARRY_PROXY_UPSTREAM_KEY", "fixture-primary")
            .env("CARRY_PROXY_CLASSIFIER_KEY", "fixture-shadow")
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
    let (_proxy, url) = start_proxy(state.path(), address, "compact").await;
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
    }
    let saved = checkpoint_states(state.path()).remove(0);
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
async fn explicit_replay_reviews_before_first_primary_without_exposing_fresh_tool_results() {
    for mode in ["compact", "audit", "off"] {
        let fixture =
            fixture(json!({"protected": ["g1"], "removable": ["g2"], "memories": []})).await;
        let state = tempfile::tempdir().unwrap();
        let (proxy, url) = start_proxy_options(
            state.path(),
            fixture.address,
            mode,
            &["--review-every-requests", "5"],
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
        let saved = checkpoint_states(state.path()).remove(0);
        assert_eq!(
            saved["shadow"]["calls"],
            if mode == "off" { 0 } else { 1 },
            "replayed history must be reviewed before request one, independent of normal cadence"
        );
        assert_eq!(saved["invalid_reviews"], 0);
        let primary = fixture.requests.lock().await[0].clone();
        assert_eq!(
            primary.to_string().contains("REPLAY_OLD_SOURCE"),
            mode != "compact"
        );
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
        assert_eq!(
            fixture.requests.lock().await[1],
            primary,
            "restart and full replay must not resurrect committed removals"
        );
        let saved = checkpoint_states(state.path()).remove(0);
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
    for _ in 0..2 {
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
        let saved = checkpoint_states(state.path()).remove(0);
        for i in 0..6 {
            assert_eq!(
                saved["history"][i]["exposed"],
                i < 3,
                "only caller-declared replay prefix is previously consumed history"
            );
        }
        assert_eq!(saved["completed_requests"], 0);
        assert_eq!(saved["invalid_reviews"], 0);
        assert!(
            !fixture
                .requests
                .lock()
                .await
                .last()
                .unwrap()
                .to_string()
                .contains("old source")
        );
    }
}

#[tokio::test]
async fn failed_rebase_bootstrap_retries_before_existing_review_cadence() {
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
        let saved = checkpoint_states(state.path()).remove(0);
        assert_eq!(saved["completed_requests"], 1);
        assert_eq!(saved["history_rebases"], 1);
        assert_eq!(saved["invalid_reviews"], 0);
        assert_eq!(
            saved["shadow"]["calls"], attempt,
            "bootstrap retry must not be skipped just because an earlier lineage had completions"
        );
        assert_eq!(saved["history"][2]["exposed"], false);
        assert_eq!(saved["bootstrap_review_pending"], true);
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
    for field in ["conversation", "background"] {
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
    let saved = checkpoint_states(state.path()).remove(0);
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
    let (_proxy, url) = start_proxy(state.path(), fixture.address, "compact").await;
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
    let saved = checkpoint_states(state.path()).remove(0);
    assert_eq!(saved["invalid_reviews"], 0);
    assert_eq!(saved["shadow"]["calls"], 1);
    assert_eq!(saved["memories"][0]["text"], "remember tested outcome");
    assert_eq!(saved["opinions"]["2"], "drop");
    assert_eq!(
        saved["compactions"], 0,
        "failed primary does not commit a speculative removal"
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
    assert!(send(&url, &next, "a", "main").await.status().is_success());
    let mut saved = checkpoint_states(state.path()).remove(0);
    for _ in 0..50 {
        if saved["completed_requests"] == 2 {
            break;
        }
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
    let (proxy, url) = start_proxy(state.path(), address, "compact").await;
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
    assert_eq!(
        send(&url, &delta, "a", "main").await.status(),
        reqwest::StatusCode::CONFLICT,
        "a stale response ID must not silently fork the session"
    );
    drop(proxy);
    let (_proxy, url) = start_proxy(state.path(), address, "compact").await;
    let resumed = json!({"model": "fixture-model", "previous_response_id": "resp_2", "input": [{"role": "user", "content": "after restart"}]});
    let response = send(&url, &resumed, "a", "main").await;
    assert!(response.status().is_success());
    let _: serde_json::Value = response.json().await.unwrap();
    let captured = requests.lock().await;
    assert_eq!(captured.len(), 3);
    assert!(captured[2].get("previous_response_id").is_none());
    assert_eq!(captured[2]["input"].as_array().unwrap().len(), 5);
    assert_eq!(captured[2]["input"][4]["content"], "after restart");
    server.abort();
}
