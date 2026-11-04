// Scripted loopback reviewer: exercises actual request construction, transport,
// advice application and current-format restart, never model adherence.
fn reminder_ledger(body: &Value) -> Value {
    serde_json::from_str(
        body["input"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

fn reminder_subject() -> Session {
    let mut session = Session::default();
    let mut input = vec![json!({"role":"system","content":"fixed"})];
    for i in 0..10 {
        input.push(json!({"role":"user","content":format!("goal {i}")}));
        input.push(json!({"role":"assistant","content":format!("evidence {i}")}));
    }
    input.extend([
        json!({"type":"reasoning","encrypted_content":"opaque"}),
        json!({"type":"function_call","call_id":"call","name":"lookup","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"call","output":"tool evidence"}),
        json!({"role":"user","content":"current task"}),
        json!({"type":"function_call","call_id":"open","name":"lookup","arguments":"{}"}),
    ]);
    session.ingest(&input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.review_context = json!({"model":"gpt-6-luna"});
    session.history_namespace = Some("reminder-fixture".into());
    session
}

#[tokio::test]
async fn old_protected_reminders_rotate_on_real_reviews_without_releasing_silent_keeps() {
    let initial_keeps = reminder_subject()
        .groups()
        .into_iter()
        .filter(|g| g.exposed && !g.pinned && g.replayable && !g.human)
        .map(|g| json!(g.id))
        .collect::<Vec<_>>();
    let (sender, mut captures) = tokio::sync::mpsc::unbounded_channel();
    let router = Router::new().route("/review", post(move |axum::Json(body): axum::Json<Value>| {
        let sender = sender.clone();
        let initial_keeps = initial_keeps.clone();
        async move {
            let ledger = reminder_ledger(&body);
            // First review protects eligible non-human groups; subsequent
            // silence must preserve these opinions, not expire them.
            let protected = if ledger["current_groups"].as_array().unwrap().iter().all(|g| g["opinion"].is_null()) {
                initial_keeps
            } else { vec![] };
            sender.send(body).unwrap();
            axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":json!({"protected":protected,"removable":[],"memories":[]}).to_string()}]}],"usage":{"input_tokens":100,"output_tokens":7}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.classifier_url =
        Some(format!("http://{}/review", listener.local_addr().unwrap()));
    service.config.review_every_requests = 1;
    service.config.review_min_primary_tokens = 0;
    service.config.review_min_new_tokens = 0;
    service.config.review_backoff_factor = 1;
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut session = reminder_subject();
    let canonical = session.render_primary();
    let eligible = session
        .groups()
        .into_iter()
        .filter(|g| g.exposed && !g.pinned && g.replayable && !g.human)
        .collect::<Vec<_>>();
    assert!(eligible.len() > 8);
    let mut seen = HashSet::new();
    for round in 0..4 {
        session.completed_requests += 1;
        assert_eq!(
            admission::scheduled(&service.config, &session)["admitted"],
            true
        );
        let projection = planning::Projection::canonical(&session);
        let prospective = review_body_projected(&service.config, &session, &projection);
        for _ in 0..3 {
            assert_eq!(
                review_body_projected(&service.config, &session, &projection),
                prospective
            );
        }
        let mut run = ReviewRun::default();
        assert!(run_review(&service, "reminders", &mut session, &projection, &mut run).await);
        let wire = tokio::time::timeout(Duration::from_secs(5), captures.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(wire, prospective);
        let ledger = reminder_ledger(&wire);
        // No old sources exist on initial ingestion. Reach a completed real
        // keep before requiring reminders, so baseline RED is old-context
        // behavior, not just a missing optional field on a fresh request.
        let reminders = if round == 0 {
            ledger
                .get("reminder_group_ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        } else {
            ledger["reminder_group_ids"]
                .as_array()
                .expect(
                    "ordinary reviewer request must carry bounded old-protected reminder targets",
                )
                .clone()
        };
        if round == 0 {
            assert!(reminders.is_empty());
        } else {
            assert!(!reminders.is_empty() && reminders.len() <= 4);
            for target in reminders {
                let id = target.as_u64().unwrap();
                assert!(
                    eligible.iter().any(|g| g.id == id),
                    "only old eligible non-human whole groups may be reminders"
                );
                assert!(
                    seen.insert(id),
                    "rotation must cover the old pool before repeating it"
                );
            }
        }
        assert_eq!(
            session.render_primary(),
            canonical,
            "reminders never alter canonical source"
        );
        assert_eq!(session.shadow.calls, round + 1);
        assert_eq!(session.shadow.input_tokens, (round + 1) * 100);
        for group in &eligible[..10] {
            assert_eq!(
                session.opinions.get(&group.id).map(String::as_str),
                Some("keep")
            );
        }
        let advice = semantic::Record::capture(&session);
        session = serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
        advice.apply(&mut session);
    }
    assert_eq!(
        seen.len(),
        eligible.len(),
        "all previously reviewed non-removable sources must get a turn"
    );
    assert!(
        captures.try_recv().is_err(),
        "reminders do not create extra reviewer calls"
    );
    provider.abort();
}
