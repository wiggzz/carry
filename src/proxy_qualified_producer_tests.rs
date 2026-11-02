// Explicit SYNTHETIC qualified-contract transport/terminal seam. The physical
// destination is loopback; only the disposable Commit's resolution is official.
// No production route exception, TLS interception or provider-residency claim.
async fn synthetic_qualified_terminal_restart(sse: bool) {
    let captures = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = captures.clone();
    let router = Router::new().route("/primary", post(move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
        let captured = captured.clone();
        async move {
            let n = {
                let mut all = captured.lock().unwrap();
                all.push((headers["authorization"].to_str().unwrap().to_owned(), body));
                all.len()
            };
            if n == 3 {
                return (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({"error":{"code":"synthetic_retry"}}))).into_response();
            }
            let mut details = match n {
                1 => json!({"cached_tokens":0,"cache_write_tokens":5000}),
                2 | 4 => json!({"cached_tokens":5000,"cache_write_tokens":0}),
                5 => json!({"cached_tokens":2500,"cache_write_tokens":0}),
                _ => json!({"cached_tokens":0,"cache_write_tokens":0}),
            };
            if n == 6 { details["cached_tokens"] = json!(6000); }
            let value = json!({"id":format!("synthetic-qualified-{n}"),"status":"completed","output":[],
                "usage":{"input_tokens":5000,"output_tokens":0,"input_tokens_details":details}});
            if sse {
                ([("content-type", "text/event-stream")], format!("data: {}\n\n", json!({"type":"response.completed","response":value}))).into_response()
            } else {
                axum::Json(value).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let mut initial = checkpoint_service(directory.path());
    initial.config.upstream_url = format!("http://{address}/primary");
    initial.config.primary_cache_auth = "synthetic-credential-identity".into();
    let resolved = resolve_model(
        "gpt-6.1-sol",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        0,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    // Catalog rates are dollars per million; settlement consumes this owner's
    // input-tier-qualified dollars-per-token conversion.
    let rates = resolved.rates_for_input(5000.0).unwrap();
    initial.config.model_resolution = Some(ResolvedPair {
        schema_version: 1,
        primary: resolved.clone(),
        reviewer: resolved.clone(),
    });
    let config = initial.config.clone();
    let body = json!({"model":"gpt-6.1-sol","instructions":"fixed synthetic overhead",
        "prompt_cache_key":"synthetic-qualified-terminal","stream":sse,"input":[
            {"role":"user","content":"stable whole prefix ".repeat(800)},
            {"type":"function_call","call_id":"whole-cohort","name":"fixture","arguments":"{}"},
            {"type":"function_call_output","call_id":"whole-cohort","output":"complete result"},
            {"role":"user","content":"current protected task"}]});
    assert_eq!(primary_wire_body(&config, &body), body);
    let mut session = Session {
        history_namespace: Some("synthetic-qualified-terminal".into()),
        model_resolution: config.model_resolution.clone(),
        ..Session::default()
    };
    session.ingest(body["input"].as_array().unwrap()).unwrap();
    save(&initial, "synthetic-qualified-terminal", &session).unwrap();
    drop(initial);
    let mut snapshots = Vec::new();
    let mut bounds = Vec::new();
    let mut paid = 0.0;
    for step in 1..=7 {
        // New Service and real checkpoint/index reload on every request. No
        // observations or read/write windows are injected into persisted state.
        let mut restarted = checkpoint_service(directory.path());
        restarted.config = config.clone();
        let service = Arc::new(restarted);
        let id = "synthetic-qualified-terminal";
        let candidate = load(&service, id).unwrap();
        let before_money = candidate.primary.cost_usd;
        let previous = history::cache(&service).unwrap();
        let lock = session_lock(&service, id).await;
        let pin = history::pin(&service, id).unwrap();
        let commit = Commit {
            service: service.clone(),
            id: id.into(),
            request_id: format!("qualified-{step}"),
            reviewed: candidate.clone(),
            submitted_ids: candidate.history.iter().map(|i| i.id).collect(),
            observed_input: body["input"].as_array().unwrap().clone(),
            candidate,
            outbound: body.clone(),
            native: false,
            dispatched_at: now(),
            _history_pin: pin,
            _lock: lock,
        };
        let start = commit.dispatched_at;
        let upstream = service
            .client
            .post(&config.upstream_url)
            .bearer_auth("synthetic-wire-key")
            .json(&body)
            .send()
            .await
            .unwrap();
        let response = relay(upstream, Some(commit), None, false, None).await;
        assert_eq!(
            response.status(),
            if step == 3 {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::OK
            }
        );
        let raw = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(!raw.is_empty());
        let saved = load(&service, id).unwrap();
        let cache = history::cache(&service).unwrap();
        assert_eq!(saved.primary.calls, step);
        assert_eq!(saved.completed_requests, step - u64::from(step >= 3));
        assert_eq!(saved.failed_primaries, u64::from(step >= 3));
        if step == 3 {
            assert_eq!(
                saved.last_failure.as_ref().unwrap()["request_id"],
                format!("qualified-{step}")
            );
        } else {
            assert!(
                saved.last_failure.is_none(),
                "a completed primary clears the current failure"
            );
        }
        assert_eq!(saved.shadow.calls, 0);
        assert_eq!(saved.compactions, 0);
        assert_eq!(
            saved.render_primary(),
            body["input"].as_array().unwrap().clone()
        );
        match step {
            1 => paid += 5000.0 * rates.write,
            2 | 4 => paid += 5000.0 * rates.cached,
            5 => paid += 2500.0 * (rates.input + rates.cached),
            7 => paid += 5000.0 * rates.input,
            _ => assert_eq!(saved.primary.cost_usd, before_money),
        }
        assert!((saved.primary.cost_usd - paid).abs() < 1e-12);
        assert_eq!(
            saved.primary.unavailable_cost_calls,
            u64::from(step >= 3) + u64::from(step >= 6)
        );
        assert_eq!(saved.primary.cache_write_tokens, 5000);
        if step == 3 {
            assert_eq!(
                serde_json::to_value(&cache).unwrap(),
                serde_json::to_value(previous).unwrap(),
                "failed read cannot change cache proof"
            );
        }
        bounds.push((start, now()));
        snapshots.push(cache);
        drop(service);
        if step == 1 {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while now() == start {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "fixture wall clock did not advance"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    provider.abort();
    let actual = captures.lock().unwrap();
    assert_eq!(actual.len(), 7);
    for (auth, wire) in actual.iter() {
        assert_eq!(auth, "Bearer synthetic-wire-key");
        assert_eq!(
            *wire, body,
            "complete final native wire, including tool cohort and fixed overhead"
        );
    }
    // All transport, raw-paid, count, whole-input and cold-reload guards finish
    // before evaluating separate read proof and future economic eligibility.
    let original = serde_json::to_value(&snapshots[0][0]).unwrap();
    let written = snapshots[0][0].write_window.unwrap();
    for step in [1usize, 2, 3] {
        assert!(
            snapshots[step]
                .iter()
                .any(|fact| serde_json::to_value(fact).unwrap() == original)
        );
    }
    let read = snapshots[1]
        .iter()
        .find(|fact| fact.read_window.is_some())
        .unwrap();
    assert_eq!(read.native_read_tokens, Some(5000));
    assert_eq!(read.native_write_tokens, Some(0));
    let window = read.read_window.unwrap();
    assert_eq!(window.lifetime, 1800);
    assert!(
        bounds[1].0 <= window.not_before
            && window.not_before <= window.not_after
            && window.not_after <= bounds[1].1
    );
    let expired_original = written.not_before + written.lifetime;
    let credit = crate::proxy_savings::token_units(&body["input"]) as f64;
    let reusable =
        |facts: &[CacheEvidence], wire: &Value, model: &ResolvedModel, auth: &str, at| {
            crate::proxy_cache::reusable(facts, wire, Some(model), auth, at)
        };
    assert_eq!(
        reusable(
            &snapshots[0],
            &body,
            &resolved,
            &config.primary_cache_auth,
            expired_original
        ),
        0.0
    );
    assert_eq!(
        reusable(
            &snapshots[1],
            &body,
            &resolved,
            &config.primary_cache_auth,
            expired_original
        ),
        credit
    );
    assert_eq!(
        reusable(
            &snapshots[1],
            &body,
            &resolved,
            &config.primary_cache_auth,
            window.not_before + 1800
        ),
        0.0
    );
    assert_eq!(
        reusable(
            &snapshots[1],
            &body,
            &resolved,
            "other-credential",
            expired_original
        ),
        0.0
    );
    for changed in [
        {
            let mut b = body.clone();
            b["instructions"] = json!("changed fixed overhead");
            b
        },
        {
            let mut b = body.clone();
            b["prompt_cache_key"] = json!("sibling");
            b
        },
        {
            let mut b = body.clone();
            b["input"][0]["content"] = json!("tiny genuine prefix edit");
            b
        },
    ] {
        assert_eq!(
            reusable(
                &snapshots[1],
                &changed,
                &resolved,
                &config.primary_cache_auth,
                expired_original
            ),
            0.0
        );
    }
    for route in [
        Route::new("https://other.invalid/v1/responses", false),
        Route::new("https://api.openai.com/v1/responses", true),
    ] {
        let mut model = resolved.clone();
        model.route = route;
        assert_eq!(
            reusable(
                &snapshots[1],
                &body,
                &model,
                &config.primary_cache_auth,
                expired_original
            ),
            0.0
        );
    }
    let mut tier = resolved.clone();
    tier.route.processing_tier = "priority".into();
    assert_eq!(
        reusable(
            &snapshots[1],
            &body,
            &tier,
            &config.primary_cache_auth,
            expired_original
        ),
        0.0
    );
    assert!(
        snapshots[3].iter().any(|fact| fact.read_window.is_some()),
        "successful caller retry produces one new read receipt"
    );
    for step in [4usize, 5, 6] {
        assert_eq!(
            reusable(
                &snapshots[step],
                &body,
                &resolved,
                &config.primary_cache_auth,
                expired_original
            ),
            0.0,
            "partial/conflicting/missing hit cannot resurrect withdrawn coverage"
        );
    }
}

#[tokio::test]
async fn synthetic_qualified_json_terminal_preserves_writes_and_cold_reload() {
    synthetic_qualified_terminal_restart(false).await;
}

#[tokio::test]
async fn synthetic_qualified_sse_terminal_preserves_writes_and_cold_reload() {
    synthetic_qualified_terminal_restart(true).await;
}
