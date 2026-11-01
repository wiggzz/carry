// Acceptance of the existing ContextState/client/CacheTracker boundaries, not
// defect reproductions or live-provider cache residency. Qualified capabilities
// and terminal write receipts below are explicitly synthetic.
fn scope_tool(state: &mut ContextState, call: &str) -> u64 {
    state.add_tool(
        vec![json!({"type":"function_call","name":"shell","call_id":call,"arguments":"{}"})],
        json!({"type":"function_call_output","call_id":call,"output":"native cohort result ".repeat(300)}),
    ).unwrap()
}

fn scope_signals(keep: &[u64], drop: &[u64]) -> crate::protocol::ContextManagement {
    crate::protocol::ContextManagement {
        keep: keep.to_vec(),
        drop: drop.to_vec(),
        remember: Vec::new(),
    }
}

#[test]
fn native_rendered_generation_and_cold_resume_use_existing_context_owner() {
    let mut context = ContextState::new("initial protected direction ".repeat(800));
    let disposable = scope_tool(&mut context, "disposable");
    let retained = scope_tool(&mut context, "retained");
    context.record_signals(&scope_signals(&[retained], &[disposable]), retained);
    let mut cache = CacheTracker::new(Some(openai_cache_capabilities()));
    let first_plan = context
        .plan_compaction(&[], cache.policy_for_history(&context.input_items(), 1, 0))
        .unwrap();
    let first_change = context.compact(first_plan);
    cache.mark_compaction(&first_change.invalidated_generations);
    let first = context.input_items();
    let first_breakpoints = context.rendered_breakpoints();
    assert_eq!(
        first_breakpoints
            .iter()
            .map(|b| b.generation)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    let first_generation = first_breakpoints.last().unwrap().generation;
    cache.begin_request_with_history(first_breakpoints, &first, context.estimated_tokens());
    cache.observe_receipt(
        &json!({"input_tokens":50000,"cached_input_tokens":0,"cache_write_input_tokens":50000}),
    );
    let warm = cache.policy_for_history(&first, 1, 0);
    assert!(warm.implicit_cached_tokens > 0);
    assert_eq!(warm.breakpoints.len(), 2);
    assert_eq!(
        warm.breakpoints.last().unwrap().generation,
        first_generation
    );
    let original = cache.breakpoints[&first_generation].write;
    context.add_user("appended task".into());
    let appended = context.input_items();
    assert!(appended.starts_with(&first));
    assert_eq!(
        cache
            .policy_for_history(&appended, 1, 0)
            .implicit_cached_tokens,
        warm.implicit_cached_tokens
    );
    assert_eq!(
        context.rendered_breakpoints().last().unwrap().generation,
        first_generation
    );
    assert_eq!(cache.breakpoints[&first_generation].write, original);

    // A real validated newer miss revokes credit before the next rewrite.
    cache.begin_request_with_history(
        context.rendered_breakpoints(),
        &appended,
        context.estimated_tokens(),
    );
    cache.observe_receipt(
        &json!({"input_tokens":50000,"cached_input_tokens":0,"cache_write_input_tokens":0}),
    );
    let latest = scope_tool(&mut context, "latest");
    context.record_signals(&scope_signals(&[latest], &[1]), latest);
    let second_plan = context
        .plan_compaction(&[], cache.policy_for_history(&context.input_items(), 1, 0))
        .unwrap();
    let second_change = context.compact(second_plan);
    cache.mark_compaction(&second_change.invalidated_generations);
    let current = context.input_items();
    assert!(!current.starts_with(&first));
    assert!(
        context
            .rendered_breakpoints()
            .iter()
            .all(|b| b.generation != first_generation)
    );
    let current_policy = cache.policy_for_history(&current, 1, 0);
    assert_eq!(current_policy.implicit_cached_tokens, 0);
    assert!(current_policy.breakpoints.is_empty());
    assert!(!current.iter().any(|item| item["call_id"] == "disposable"));
    for call in ["retained", "latest"] {
        assert_eq!(
            current
                .iter()
                .filter(|item| item["call_id"] == call)
                .count(),
            2,
            "whole native call/result cohort survives"
        );
    }

    // Current-format context restores exact rendering, not runtime cache facts.
    let resumed = ContextState::decode(&context.encode().unwrap()).unwrap();
    assert_eq!(resumed.input_items(), current);
    let cold = CacheTracker::new(Some(openai_cache_capabilities()));
    let policy = cold.policy_for_history(&resumed.input_items(), 1, 0);
    assert_eq!(policy.implicit_cached_tokens, 0);
    assert!(policy.breakpoints.is_empty());
}

#[tokio::test]
async fn native_retry_preserves_authenticated_wire_and_one_original_write_interval() {
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    let (send, mut captured) = tokio::sync::mpsc::unbounded_channel();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let router = Router::new().route("/responses", post(move |headers: HeaderMap, Json(body): Json<Value>| {
        let first = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        send.send((headers, body)).unwrap();
        async move {
            if first {
                (StatusCode::TOO_MANY_REQUESTS, [("retry-after-ms", "1")], Json(json!({"error":{"code":"rate_limit_exceeded"}})))
            } else {
                (StatusCode::OK, [("retry-after-ms", "1")], Json(json!({"id":"synthetic-retried",
                    "output":[{"type":"function_call","name":"finish","call_id":"finish-1",
                        "arguments":"{\"answer\":\"done\",\"context\":{\"protected\":[],\"removable\":[],\"remember\":[]}}"}],
                    "usage":{"input_tokens":5000,"output_tokens":4,"cached_input_tokens":0,"cache_write_input_tokens":5000}})))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = OpenAiClient::with_timeouts_and_prompt_cache_key(
        base,
        crate::openai::RequestAuth::ApiKey("synthetic-only".into()),
        "gpt-6-sol".into(),
        "medium".into(),
        "synthetic-native-scope".into(),
        Duration::from_secs(5),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(
        client.prompt_cache_capabilities().is_none(),
        "loopback does not attest vendor entitlement"
    );
    let history = ContextState::new("retained exact native history ".repeat(400));
    let input = history.input_items();
    let mut cache = CacheTracker::new(Some(openai_cache_capabilities()));
    cache.begin_request_with_history(
        history.rendered_breakpoints(),
        &input,
        history.estimated_tokens(),
    );
    let (started, lifetime) = cache.pending_write.unwrap();
    let reply = client.step("fixed synthetic system", &input).await;
    server.abort();
    let reply = reply.unwrap();
    let first = captured.try_recv().unwrap();
    let second = captured.try_recv().unwrap();
    assert!(captured.try_recv().is_err());
    assert_eq!(first.0["authorization"], "Bearer synthetic-only");
    assert_eq!(second.0["authorization"], first.0["authorization"]);
    assert_eq!(first.1, second.1, "retries reuse complete outgoing body");
    let mut expected = client.request_body("fixed synthetic system", &input);
    expected["stream"] = json!(true);
    assert_eq!(first.1, expected);
    assert_eq!(reply.response_retries, 1);
    assert_eq!(reply.raw["usage"]["input_tokens"], 5000);
    assert_eq!(reply.raw["usage"]["cache_write_input_tokens"], 5000);
    assert!(
        (crate::openai::receipt_cost_usd("gpt-6-sol", &reply.raw).unwrap() - 0.01254).abs() < 1e-12
    );
    assert_eq!(
        cache
            .policy_for_history(&input, 1, 0)
            .implicit_cached_tokens,
        0,
        "a request/retry is not a settled write"
    );
    assert_eq!(cache.pending_write, Some((started, lifetime)));
    cache.observe_receipt(&reply.raw["usage"]);
    let write = cache.implicit_write.unwrap();
    assert_eq!(write.origin, started);
    assert!(write.eligible_at(Instant::now()));
    assert!(!write.eligible_at(started + lifetime));
    assert_eq!(
        cache
            .policy_for_history(&input, 1, 0)
            .implicit_cached_tokens,
        history.estimated_tokens()
    );
    assert_eq!(history.input_items(), input);
}
