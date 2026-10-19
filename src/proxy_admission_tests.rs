// Model-free, real HTTP scheduler regressions; no production substitutes.
use super::*;

async fn admission_fixture(
    advice: Value,
) -> (tempfile::TempDir, Service, tokio::task::JoinHandle<()>) {
    let directory = tempfile::tempdir().unwrap();
    let router = Router::new().route("/responses", post(move || {
        let advice = advice.clone();
        async move { axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}],"usage":{"input_tokens":100,"output_tokens":10}})) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut service = checkpoint_service(directory.path());
    service.config =
        ProxyCli::try_parse_from(["carry proxy", "--background-reviews=false"]).unwrap();
    service.config.state_dir = directory.path().to_path_buf();
    service.config.classifier_url = Some(url);
    (directory, service, provider)
}

fn admission_session(tokens: usize, completed: u64) -> Session {
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"original goal"}),
            json!({"role":"assistant","content":" payload".repeat(tokens)}),
            json!({"role":"user","content":"current goal"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.completed_requests = completed;
    session
}

fn admission_main(session: &Session) -> Value {
    json!({"model":"gpt-6-luna","input":session.render_primary()})
}

#[tokio::test]
async fn adaptive_default_floor_skips_small_retained_primary_without_paid_review() {
    let (_directory, service, provider) =
        admission_fixture(json!({"protected":[],"removable":[],"memories":[]})).await;
    let mut session = admission_session(100, 4);
    let main = admission_main(&session);
    assert!(
        !review(
            &service,
            "floor",
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await,
        "ordinary review below the retained primary floor must be skipped"
    );
    assert_eq!(session.shadow.calls, 0);
    provider.abort();
}

#[tokio::test]
async fn adaptive_default_requires_four_completed_requests_even_with_large_source() {
    let (_directory, service, provider) =
        admission_fixture(json!({"protected":[],"removable":[],"memories":[]})).await;
    let mut session = admission_session(151000, 3);
    let main = admission_main(&session);
    assert!(
        !review(
            &service,
            "cadence",
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await,
        "large native growth does not replace the completed-request batch"
    );
    assert_eq!(session.shadow.calls, 0);
    session.completed_requests = 4;
    assert!(
        review(
            &service,
            "cadence",
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    assert_eq!(session.shadow.calls, 1);
    provider.abort();
}

#[tokio::test]
async fn adaptive_no_yield_backoff_survives_checkpoint_and_requires_both_batches() {
    let (_directory, service, provider) =
        admission_fixture(json!({"protected":[],"removable":[],"memories":[]})).await;
    let mut session = admission_session(151000, 4);
    let main = admission_main(&session);
    assert!(
        review(
            &service,
            "backoff",
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    let mut input = session
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    input.push(json!({"role":"assistant","content":" addition".repeat(26000)}));
    input.push(json!({"role":"user","content":"next task"}));
    session.ingest(&input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.completed_requests = 8;
    save(&service, "backoff", &session).unwrap();
    let mut resumed = load(&service, "backoff").unwrap();
    let main = admission_main(&resumed);
    assert!(
        !review(
            &service,
            "backoff",
            &mut resumed,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await,
        "one valid no-yield doubles BOTH request and native-token batches across restart"
    );
    assert_eq!(resumed.shadow.calls, 1);
    provider.abort();
}

#[test]
fn adaptive_planner_does_not_invent_native_growth_in_frozen_source_forecast() {
    let config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    let session = admission_session(100, 4);
    let (_, report) = plan(&config, &session, &admission_main(&session));
    assert_eq!(
        report["future_reviewer_calls"], 0,
        "frozen small retained source never crosses the primary admission floor"
    );
}
