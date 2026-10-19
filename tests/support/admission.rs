use super::*;

#[test]
fn admission_invalid_cli_and_environment_values_fail_before_state_side_effects() {
    for (flag, env_name, bad) in [
        (
            "--review-min-primary-tokens",
            "CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS",
            "2000001",
        ),
        (
            "--review-min-new-tokens",
            "CARRY_PROXY_REVIEW_MIN_NEW_TOKENS",
            "-1",
        ),
        (
            "--review-every-requests",
            "CARRY_PROXY_REVIEW_EVERY_REQUESTS",
            "0",
        ),
        (
            "--review-backoff-factor",
            "CARRY_PROXY_REVIEW_BACKOFF_FACTOR",
            "0",
        ),
        (
            "--review-backoff-max",
            "CARRY_PROXY_REVIEW_BACKOFF_MAX",
            "65",
        ),
    ] {
        for cli in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let state = dir.path().join("not-created");
            let mut command = Command::new(env!("CARGO_BIN_EXE_carry"));
            command.args(["proxy", "--state-dir", state.to_str().unwrap()]);
            if cli {
                command.arg(format!("{flag}={bad}"));
            } else {
                command.env(env_name, bad);
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(2),
                "invalid {flag} must fail Clap validation"
            );
            assert!(!state.exists());
        }
    }
}

#[tokio::test]
async fn admission_effective_env_policy_cli_precedence_and_actual_forwarding() {
    use axum::{Router, routing::post};
    let router=Router::new().route("/v1/responses",post(||async {
        axum::Json(json!({"id":"admission-primary","status":"completed","output":[],"usage":{"input_tokens":10,"output_tokens":1}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let state = tempfile::tempdir().unwrap();
    let (_proxy, url) = start_proxy_options_env(
        state.path(),
        address,
        "compact",
        &["--review-backoff-factor", "3"],
        "",
        &[
            ("CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS", "150000"),
            ("CARRY_PROXY_REVIEW_MIN_NEW_TOKENS", "25000"),
            ("CARRY_PROXY_REVIEW_EVERY_REQUESTS", "4"),
            ("CARRY_PROXY_REVIEW_BACKOFF_FACTOR", "2"),
            ("CARRY_PROXY_REVIEW_BACKOFF_MAX", "8"),
        ],
    )
    .await;
    for _ in 0..6 {
        let response = send(
            &url,
            &json!({"model":"gpt-6-luna","input":[{"role":"user","content":"small task"}]}),
            "adaptive-native",
            "main",
        )
        .await;
        assert!(response.status().is_success());
    }
    let metrics: serde_json::Value = reqwest::Client::new()
        .get(format!("{url}/carry/metrics"))
        .header("x-carry-session", "adaptive-native")
        .header("x-carry-branch", "main")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metrics["completed_requests"], 6);
    assert_eq!(metrics["shadow"]["calls"], 0);
    assert_eq!(
        metrics["review_policy"],
        json!({"min_primary_tokens":150000,"min_new_tokens":25000,"every_requests":4,"backoff_factor":3,"backoff_max":8})
    );
    server.abort();
}
