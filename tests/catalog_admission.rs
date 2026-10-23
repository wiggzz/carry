//! Synthetic metadata and receipts only; exercises the production proxy binary.
use axum::{
    Router,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    net::SocketAddr,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Mutex;

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn start(
    home: &Path,
    state: &Path,
    address: SocketAddr,
    subscription: bool,
    args: &[&str],
) -> (Child, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_carry"));
    command
        .args([
            "proxy",
            "--listen",
            "127.0.0.1:0",
            "--no-open-dashboard",
            "--upstream-url",
            &format!("http://{address}/v1/responses"),
            "--state-dir",
        ])
        .arg(state)
        .args([
            "--context-margin-percent",
            "0",
            "--background-reviews=false",
            "--bootstrap-min-input-bytes",
            "999999",
        ])
        .env_remove("OPENAI_API_KEY")
        .env_remove("CARRY_PROXY_CLASSIFIER_KEY")
        .env("CARRY_PROXY_UPSTREAM_KEY", "synthetic-key")
        .env("CARRY_PROXY_AUTH_TOKEN", "synthetic-client")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if subscription {
        command.arg("--codex-login").arg("--codex-home").arg(home);
    }
    command.args(args);
    let mut child = Child(command.spawn().unwrap());
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
    assert_eq!(
        reqwest::get(format!("{url}/health"))
            .await
            .unwrap()
            .status(),
        200
    );
    (child, url)
}
struct Fixture {
    home: tempfile::TempDir,
    metadata: Arc<Mutex<Value>>,
    primary_calls: Arc<AtomicUsize>,
    review_calls: Arc<AtomicUsize>,
    metadata_calls: Arc<AtomicUsize>,
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let token = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(
                r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}}"#
            )
        );
        std::fs::write(home.path().join("auth.json"), serde_json::to_vec(&json!({"version":1,"access_token":token,"refresh_token":"synthetic-refresh","expires_at_ms":4102444800000u64})).unwrap()).unwrap();
        let metadata = Arc::new(Mutex::new(json!({"models":[
            {"slug":"gpt-6-sol","visibility":"list","context_window":40000,"max_context_window":80000},
            {"slug":"gpt-6-luna","visibility":"list","context_window":6000,"max_context_window":12000}
        ]})));
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let review_calls = Arc::new(AtomicUsize::new(0));
        let metadata_calls = Arc::new(AtomicUsize::new(0));
        let m = metadata.clone();
        let mc = metadata_calls.clone();
        let pc = primary_calls.clone();
        let rc = review_calls.clone();
        let router = Router::new().route("/v1/models", get(move || { let m=m.clone();let mc=mc.clone();async move { mc.fetch_add(1,Ordering::SeqCst);axum::Json(m.lock().await.clone()) }}))
            .route("/v1/responses", post(move |axum::Json(body): axum::Json<Value>| { let pc=pc.clone();let rc=rc.clone();async move {
                let reviewer = body["model"] == "gpt-6-luna";
                if reviewer { rc.fetch_add(1,Ordering::SeqCst); } else { pc.fetch_add(1,Ordering::SeqCst); }
                let output = if reviewer { json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"protected\":[],\"removable\":[],\"memories\":[]}"}]}]) } else {json!([])};
                let response = json!({"id":"synthetic-answer","status":"completed","output":output,"usage":{"input_tokens":100,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}});
                if body["stream"] == true {
                    axum::response::IntoResponse::into_response(([ ("content-type","text/event-stream") ],format!("data: {}\n\n",json!({"type":"response.completed","response":response}))))
                } else { axum::response::IntoResponse::into_response(axum::Json(response)) }
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            home,
            metadata,
            primary_calls,
            review_calls,
            metadata_calls,
            address,
            task,
        }
    }
}
async fn catalog(url: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!("{url}/carry/catalog?model=gpt-6-sol"))
        .bearer_auth("synthetic-client")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

#[tokio::test]
async fn restarted_primary_receipts_and_scenarios_use_frozen_prices_including_unknown() {
    let f = Fixture::new().await;
    for unknown in [false, true] {
        let state = tempfile::tempdir().unwrap();
        let (child, url) = start(f.home.path(), state.path(), f.address, true, &[]).await;
        let body = json!({"model":"gpt-6-sol","input":[{"role":"user","content":"synthetic frozen economics"}],"max_output_tokens":10});
        assert_eq!(send(&url, "frozen", &body).await, 200);
        drop(child);
        let (path, mut checkpoint) = saved(state.path());
        let prior_native = checkpoint["primary"]["cost_usd"].as_f64().unwrap();
        let prior_unavailable = checkpoint["primary"]["unavailable_cost_calls"]
            .as_u64()
            .unwrap();
        let prior_scenario = checkpoint["cost_trajectory"]["result"]["carry_cost"]
            .as_f64()
            .unwrap();
        let prior_excluded = checkpoint["cost_trajectory"]["result"]["excluded_unpriced_calls"]
            .as_u64()
            .unwrap();
        checkpoint["model_resolution"]["primary"]["rates"] = if unknown {
            Value::Null
        } else {
            json!({"input":7.0,"cached":0.7,"write":9.0,"output":21.0})
        };
        checkpoint["model_resolution"]["primary"]["price_basis"] = json!("synthetic_fixture_only");
        std::fs::write(path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
        let (_child, url) = start(f.home.path(), state.path(), f.address, true, &[]).await;
        assert_eq!(send(&url, "frozen", &body).await, 200);
        let (_, checkpoint) = saved(state.path());
        let delta = checkpoint["primary"]["cost_usd"].as_f64().unwrap() - prior_native;
        if unknown {
            assert_eq!(delta, 0.0, "unpriced receipt adds no invented slug price");
            assert_eq!(
                checkpoint["primary"]["unavailable_cost_calls"],
                prior_unavailable + 1
            );
            assert_eq!(
                checkpoint["cost_trajectory"]["result"]["excluded_unpriced_calls"],
                prior_excluded + 1
            );
            assert_eq!(
                checkpoint["cost_trajectory"]["result"]["carry_cost"],
                prior_scenario
            );
        } else {
            assert!(
                (delta - 721.0 / 1e6).abs() < 1e-12,
                "actual receipt must use frozen input/output tariff: {delta}"
            );
            assert!(
                checkpoint["cost_trajectory"]["result"]["carry_cost"]
                    .as_f64()
                    .unwrap()
                    > prior_scenario,
                "unknown cache capability must not prevent a matched uncached priced comparison"
            );
        }
    }
}
async fn send(url: &str, session: &str, body: &Value) -> u16 {
    let response = reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .bearer_auth("synthetic-client")
        .header("x-carry-session", session)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let _ = response.bytes().await.unwrap();
    status
}
fn saved(state: &Path) -> (std::path::PathBuf, Value) {
    let files = std::fs::read_dir(state)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    (
        files[0].clone(),
        serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap(),
    )
}

async fn assert_dashboard_capacity(url: &str, primary: u64, reviewer: u64) {
    let response = reqwest::get(format!("{url}/carry/dashboard/stats"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let stats: Value = response.json().await.unwrap();
    let sessions = stats["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0]["planner"]["primary_context_tokens"], primary,
        "dashboard denominator must describe the saved request plan"
    );
    assert_eq!(sessions[0]["planner"]["reviewer_context_tokens"], reviewer);
}

#[tokio::test]
async fn missing_subscription_entry_uses_one_catalog_for_reviewer_plan_dashboard_and_restart() {
    let f = Fixture::new().await;
    // Reproduce the deployed mismatch: inference uses GPT-6 IDs, while model
    // discovery lists only the older family. These are not aliases.
    *f.metadata.lock().await = json!({"models":[
        {"slug":"gpt-5.6-luna","visibility":"list","context_window":272000}
    ]});
    let state = tempfile::tempdir().unwrap();
    let mut body = json!({"model":"gpt-6.1-sol","input":[
        {"role":"user","content":"retained goal"},
        {"role":"assistant","content":"completed synthetic source"},
        {"role":"user","content":"next task"}
    ],"max_output_tokens":10});
    let args = [
        "--review-replayed-history",
        "--review-every-requests",
        "1",
        "--review-min-primary-tokens",
        "0",
        "--review-min-new-tokens",
        "0",
    ];
    let (child, url) = start(f.home.path(), state.path(), f.address, true, &args).await;
    let c = catalog(&url).await;
    assert_eq!(
        c["reviewer"]["capacity_tokens"], 1050000,
        "known reviewer must resolve its catalog capacity, not the 300K role fallback"
    );
    assert!(
        c["reviewer"]["raw"]["context_window"].is_null(),
        "catalog policy must not fabricate an upstream discovery fact"
    );
    assert_eq!(c["reviewer"]["ordinary"]["total_budget"], 272000);
    assert_eq!(send(&url, "unified", &body).await, 200);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (_, checkpoint) = saved(state.path());
            if checkpoint["background_review_pending"] == false
                && checkpoint["shadow"]["calls"].as_u64().unwrap_or(0) > 0
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("reviewer must actually run and settle");
    assert_dashboard_capacity(&url, 1050000, 1050000).await;
    let (path, checkpoint) = saved(state.path());
    assert_eq!(checkpoint["last_plan"]["reviewer_context_tokens"], 1050000);
    assert_eq!(
        checkpoint["model_resolution"]["reviewer"]["rates"],
        c["reviewer"]["rates"]
    );
    assert!(f.review_calls.load(Ordering::SeqCst) > 0);
    drop(child);
    // A restart, an old fallback snapshot, and an unresolved legacy checkpoint
    // must all use the current catalog for FUTURE policy, not reprice the past.
    for legacy in [false, true] {
        let mut restored = checkpoint.clone();
        if legacy {
            restored.as_object_mut().unwrap().remove("model_resolution");
        } else {
            for (role, capacity) in [("primary", 272000), ("reviewer", 300000)] {
                restored["model_resolution"][role]["capacity_tokens"] = json!(capacity);
                restored["model_resolution"][role]["provenance"]["capacity"]["status"] =
                    json!("fallback");
                restored["model_resolution"][role]["provenance"]["capacity"]["source"] =
                    json!("conservative-role-capacity-v1");
            }
        }
        std::fs::write(&path, serde_json::to_vec(&restored).unwrap()).unwrap();
        let (child, url) = start(f.home.path(), state.path(), f.address, true, &args).await;
        assert_dashboard_capacity(&url, 1050000, 1050000).await;
        body["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"continue"}));
        assert_eq!(send(&url, "unified", &body).await, 200);
        let (_, after) = saved(state.path());
        assert_eq!(
            after["model_resolution"]["reviewer"]["capacity_tokens"],
            1050000
        );
        assert_eq!(after["last_plan"]["reviewer_context_tokens"], 1050000);
        assert_dashboard_capacity(&url, 1050000, 1050000).await;
        assert!(
            after["primary"]["cost_usd"].as_f64().unwrap()
                >= restored["primary"]["cost_usd"].as_f64().unwrap()
        );
        drop(child);
    }
}

#[tokio::test]
async fn advertised_capacity_controls_real_dispatch_and_survives_restart() {
    let f = Fixture::new().await;
    let state = tempfile::tempdir().unwrap();
    let (child, url) = start(f.home.path(), state.path(), f.address, true, &[]).await;
    let mut body = json!({"model":"gpt-6-sol","input":[{"role":"user","content":"synthetic capacity boundary"}],"max_output_tokens":30000});
    assert_eq!(send(&url, "one", &body).await, 200);
    let (_, checkpoint) = saved(state.path());
    assert_eq!(
        checkpoint["last_plan"]["primary_context_tokens"], 40000,
        "catalog capacity must reach the real admission gate"
    );
    assert_eq!(checkpoint["last_plan"]["reviewer_context_tokens"], 6000);
    assert_dashboard_capacity(&url, 40000, 6000).await;
    assert_eq!(
        checkpoint["model_resolution"]["primary"]["capacity_tokens"],
        40000
    );
    let input = checkpoint["last_plan"]["primary_estimated_tokens"]
        .as_f64()
        .unwrap() as u64;
    for (delta, status) in [(-1, 200), (0, 200), (1, 413)] {
        body["max_output_tokens"] = json!((40000 - input) as i64 + delta);
        let before = f.primary_calls.load(Ordering::SeqCst);
        assert_eq!(
            send(&url, "one", &body).await,
            status,
            "exact capacity boundary delta={delta}"
        );
        assert_eq!(
            f.primary_calls.load(Ordering::SeqCst),
            before + usize::from(status == 200)
        );
    }
    assert_eq!(
        f.metadata_calls.load(Ordering::SeqCst),
        1,
        "no candidate/request metadata fetch loop"
    );
    let (path, checkpoint) = saved(state.path());
    let before_cost = checkpoint["primary"]["cost_usd"].clone();
    drop(child);
    f.metadata.lock().await["models"][0]["context_window"] = json!(60000);
    let (child, url) = start(f.home.path(), state.path(), f.address, true, &[]).await;
    body["max_output_tokens"] = json!(40001 - input);
    assert_eq!(
        send(&url, "one", &body).await,
        413,
        "saved route epoch must not silently adopt new metadata"
    );
    assert_dashboard_capacity(&url, 40000, 6000).await;
    let (_, mut checkpoint) = saved(state.path());
    assert_eq!(
        checkpoint["primary"]["cost_usd"], before_cost,
        "no historical repricing on restart"
    );
    assert_eq!(
        checkpoint["model_resolution"]["primary"]["capacity_tokens"],
        40000
    );
    drop(child);
    checkpoint
        .as_object_mut()
        .unwrap()
        .remove("model_resolution");
    std::fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let (_child, url) = start(f.home.path(), state.path(), f.address, true, &[]).await;
    assert_eq!(
        send(&url, "one", &body).await,
        200,
        "only new provenance absent means contemporaneous legacy capacity, not today's catalog"
    );
    let (_, checkpoint) = saved(state.path());
    assert_eq!(checkpoint["last_plan"]["primary_context_tokens"], 272000);
    assert_dashboard_capacity(&url, 272000, 300000).await;
    assert_eq!(
        checkpoint["model_resolution"]["primary"]["provenance"]["capacity"]["status"],
        "legacy_snapshot_missing"
    );
}

#[tokio::test]
async fn explicit_same_default_lower_and_alias_limits_retain_provenance() {
    let f = Fixture::new().await;
    for (args, primary, reviewer) in [
        (vec![], 272000, 300000),
        (
            vec![
                "--primary-context-tokens",
                "272000",
                "--classifier-max-input-tokens",
                "300000",
            ],
            272000,
            300000,
        ),
        (
            vec![
                "--primary-context-tokens",
                "24000",
                "--reviewer-context-tokens",
                "7000",
            ],
            24000,
            7000,
        ),
    ] {
        let state = tempfile::tempdir().unwrap();
        let (_child, url) = start(f.home.path(), state.path(), f.address, false, &args).await;
        let c = catalog(&url).await;
        assert_eq!(c["primary"]["capacity_tokens"], primary);
        assert_eq!(c["reviewer"]["capacity_tokens"], reviewer);
        for (role, value) in [("primary", primary), ("reviewer", reviewer)] {
            assert_eq!(
                c[role]["provenance"]["capacity"]["operator_override"],
                if args.is_empty() {
                    Value::Null
                } else {
                    json!(value)
                },
                "explicit same-valued defaults must differ from omission"
            );
            assert!(
                c[role]["cache"].is_null(),
                "unknown gateways inherit no official cache support"
            );
        }
    }
    assert_eq!(
        f.metadata_calls.load(Ordering::SeqCst),
        0,
        "API gateway cannot use subscription metadata"
    );
}

#[tokio::test]
async fn reviewer_route_is_independent_in_actual_admission_and_dispatch() {
    let f = Fixture::new().await;
    let state = tempfile::tempdir().unwrap();
    let reviewer = format!("http://{}/v1/responses", f.address);
    let (_child, url) = start(
        f.home.path(),
        state.path(),
        f.address,
        true,
        &["--classifier-url", &reviewer, "--review-replayed-history"],
    )
    .await;
    let body = json!({"model":"gpt-6-sol","input":[{"role":"user","content":"earlier"},{"role":"assistant","content":"completed synthetic historical work"},{"role":"user","content":"next task"}],"max_output_tokens":10});
    assert_eq!(send(&url, "one", &body).await, 200);
    // The primary is independent of reviewer completion, including the legacy
    // synchronous setting. Wait only in this fixture, after consuming primary.
    let checkpoint = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (_, checkpoint) = saved(state.path());
            if checkpoint["background_review_pending"] == false
                && checkpoint["shadow"]["calls"].as_u64().unwrap_or(0) > 0
            {
                break checkpoint;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real reviewer must settle after primary, without a production wait");
    assert_eq!(checkpoint["last_plan"]["primary_context_tokens"], 40000);
    assert_eq!(
        checkpoint["last_plan"]["reviewer_context_tokens"], 300000,
        "explicit API reviewer URL must not inherit primary subscription limit"
    );
    assert_eq!(
        checkpoint["model_resolution"]["reviewer"]["route"]["class"],
        "unknown_gateway"
    );
    assert!(
        f.review_calls.load(Ordering::SeqCst) > 0,
        "verify a real reviewer dispatch, not export decoration"
    );
}
