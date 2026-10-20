//! Credential-free, synthetic route metadata: no vendor capacity/tariff evidence.
use axum::{
    Router,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn catalog_exports_bounded_route_facts_without_inference_or_secrets() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inference = calls.clone();
    let router = Router::new()
        .route(
            "/v1/models",
            get(|| async {
                axum::Json(json!({"models":[
                    {"slug":"synthetic-route-model","visibility":"list","context_window":64000,
                     "max_context_window":128000,"auto_compact_token_limit":50000,
                     "effective_context_window_percent":95,"comp_hash":"synthetic-v1",
                     "base_instructions":"SECRET-PROMPT-DO-NOT-EXPORT"},
                    {"slug":"hidden","visibility":"hide","context_window":999999}
                ]}))
            }),
        )
        .route(
            "/v1/responses",
            post(move || {
                let calls = inference.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"id":"unused"}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let token = format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"SECRET-ACCOUNT"}}"#)
    );
    std::fs::write(home.path().join("auth.json"), serde_json::to_vec(&json!({
        "version":1,"access_token":token,"refresh_token":"SECRET-REFRESH","expires_at_ms":4102444800000u64
    })).unwrap()).unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut child = Child(
        Command::new(env!("CARGO_BIN_EXE_carry"))
            .args([
                "proxy",
                "--listen",
                "127.0.0.1:0",
                "--no-open-dashboard",
                "--codex-login",
                "--codex-home",
            ])
            .arg(home.path())
            .arg("--state-dir")
            .arg(state.path())
            .args([
                "--upstream-url",
                &format!("http://{address}/v1/responses"),
                "--mode",
                "off",
            ])
            .env_remove("OPENAI_API_KEY")
            .env_remove("CARRY_PROXY_UPSTREAM_KEY")
            .env("CARRY_PROXY_AUTH_TOKEN", "synthetic-client")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (ready, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut ready = Some(ready);
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if let Some(address) = line.strip_prefix("CARRY_PROXY_LISTEN ")
                && let Some(ready) = ready.take()
            {
                let _ = ready.send(format!("http://{address}"));
            }
        }
    });
    let url = tokio::time::timeout(std::time::Duration::from_secs(30), receiver)
        .await
        .unwrap()
        .unwrap();
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{url}/health"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let endpoint = format!("{url}/carry/catalog?model=synthetic-route-model");
    let response = client
        .get(&endpoint)
        .bearer_auth("synthetic-client")
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "healthy proxy must serve the neutral route-aware catalog"
    );
    let catalog: Value = response.json().await.unwrap();
    assert_eq!(catalog["schema_version"], 1);
    assert_eq!(catalog["primary"]["model"], "synthetic-route-model");
    assert_eq!(catalog["primary"]["raw"]["context_window"], 64000);
    assert_eq!(catalog["primary"]["raw"]["max_context_window"], 128000);
    assert_eq!(catalog["primary"]["raw"]["auto_compact_token_limit"], 50000);
    assert_eq!(
        catalog["primary"]["raw"]["effective_context_window_percent"],
        95
    );
    assert_eq!(catalog["primary"]["raw"]["comp_hash"], "synthetic-v1");
    assert_eq!(
        catalog["primary"]["capacity_tokens"], 64000,
        "maximum override is not default capacity"
    );
    assert_eq!(catalog["primary"]["price_basis"], "unknown");
    assert!(catalog["primary"]["rates"].is_null());
    for secret in [
        "SECRET-PROMPT",
        "SECRET-ACCOUNT",
        "SECRET-REFRESH",
        "access_token",
    ] {
        assert!(!catalog.to_string().contains(secret));
    }
    for key in [None, Some("wrong")] {
        let request = client.get(&endpoint);
        let request = if let Some(key) = key {
            request.bearer_auth(key)
        } else {
            request
        };
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    let again: Value = client
        .get(&endpoint)
        .bearer_auth("synthetic-client")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        again, catalog,
        "same route/model epoch is frozen, not fetched per candidate"
    );
    let discovery: Value = client
        .get(format!("{url}/v1/models"))
        .bearer_auth("synthetic-client")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        discovery,
        json!({"object":"list","data":[{"id":"synthetic-route-model","object":"model","created":0,"owned_by":"openai"}]})
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}
