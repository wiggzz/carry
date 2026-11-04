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

#[test]
fn reminder_targets_require_old_complete_exposure_and_current_removal_authority() {
    let config = reviewer_config();
    let mut session = reminder_subject();
    let groups = session.groups();
    admission::cover(&mut session, &groups);
    let eligible = groups
        .iter()
        .filter(|g| g.exposed && !g.pinned && g.replayable && !g.human)
        .collect::<Vec<_>>();
    let dropped = eligible[0];
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[dropped.id],"memories":[]}),
        )
        .unwrap();
    let unseen = eligible[1];
    session
        .history
        .iter_mut()
        .find(|i| i.id == unseen.members[0])
        .unwrap()
        .exposed = false;
    let cohort = eligible.iter().find(|g| g.members.len() > 1).unwrap();
    // A previously reviewed prefix does not cover a newly completed tool group.
    session
        .review_admission
        .as_mut()
        .unwrap()
        .source_ids
        .remove(cohort.members.last().unwrap());
    let mut input = session
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    input.extend([
        json!({"role":"user","content":"new direction"}),
        json!({"role":"assistant","content":"new evidence"}),
    ]);
    session.ingest(&input).unwrap();
    let new = session.history.last_mut().unwrap();
    new.exposed = true;
    let new_id = new.id;
    let targets = reminder_ledger(&review_body(&config, &session))["reminder_group_ids"].clone();
    let expected = eligible
        .iter()
        .filter(|g| g.id != dropped.id && g.id != unseen.id && g.id != cohort.id)
        .take(4)
        .map(|g| g.id)
        .collect::<Vec<_>>();
    assert_eq!(targets, json!(expected));
    for invalid in [dropped.id, unseen.id, cohort.id, new_id] {
        assert!(!targets.as_array().unwrap().contains(&json!(invalid)));
    }
    // A projected omission cannot turn retained provenance into a reminder.
    let omitted = dropped.members.iter().copied().collect();
    let projection = planning::Projection {
        input: session.render_projection(&omitted),
        omitted,
    };
    let targets = reminder_ledger(&review_body_projected(&config, &session, &projection))["reminder_group_ids"].clone();
    assert!(!targets.as_array().unwrap().contains(&json!(dropped.id)));
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

// Acceptance controls for the unchanged scheduler and background publication.
#[tokio::test]
async fn reminders_preserve_background_failure_pending_and_explicit_cohort_authority() {
    let (sender, mut captures) = tokio::sync::mpsc::unbounded_channel();
    let router = Router::new().route("/review", post(move |axum::Json(body): axum::Json<Value>| {
        let sender = sender.clone();
        async move {
            let (reply, response) = oneshot::channel::<Option<Value>>();
            sender.send((body, reply)).unwrap();
            match response.await.unwrap() {
                Some(advice) => axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}],"usage":{"input_tokens":100,"output_tokens":7}})).into_response(),
                None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/review", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let make_service = || {
        let mut service = checkpoint_service(directory.path());
        service.config.classifier_url = Some(url.clone());
        service.config.review_every_requests = 1;
        service.config.review_min_primary_tokens = 0;
        service.config.review_min_new_tokens = 0;
        service.config.review_backoff_factor = 1;
        service
    };
    let mut service = Arc::new(make_service());
    let id = "reminder-background";
    let mut session = reminder_subject();
    session.history_shared = true;
    // Staged completed coverage/advice; the first tracer separately obtains
    // protection and coverage from an actual completed HTTP review.
    let groups = session.groups();
    let eligible = groups
        .iter()
        .filter(|g| !g.human && !g.pinned && g.replayable)
        .cloned()
        .collect::<Vec<_>>();
    session.apply_advice(&groups, &json!({"protected":eligible.iter().map(|g| g.id).collect::<Vec<_>>(),"removable":[],"memories":[]})).unwrap();
    admission::cover(&mut session, &groups);
    history::publish(&service, &session, &session).unwrap();
    save(&service, id, &session).unwrap();
    let old_canonical = session.render_primary();
    let first =
        reminder_ledger(&review_body(&service.config, &session))["reminder_group_ids"].clone();
    assert_eq!(first.as_array().unwrap().len(), 4);
    // Cadence and oversized preparation must not consume the next batch.
    let projection = planning::Projection::canonical(&session);
    dispatch_review_for(
        service.clone(),
        id.into(),
        &mut session,
        projection,
        "not-due".into(),
    )
    .await;
    assert!(!session.background_review_pending && captures.try_recv().is_err());
    session.completed_requests += 1;
    let mut tiny = make_service();
    tiny.config.classifier_max_input_tokens = 1;
    let mut run = ReviewRun::default();
    let projection = planning::Projection::canonical(&session);
    assert!(!run_review(&tiny, id, &mut session, &projection, &mut run).await);
    assert_eq!(
        reminder_ledger(&review_body(&service.config, &session))["reminder_group_ids"],
        first
    );
    assert_eq!(session.shadow.calls, 0);
    assert!(captures.try_recv().is_err());
    let mut dropped = HashSet::new();
    for round in 0..5 {
        // Restart the actual service and reload separate indexed advice.
        service = Arc::new(make_service());
        save(&service, id, &session).unwrap();
        session = load_existing(&service, id).unwrap();
        if let Some(record) =
            history::advice(&service, "reminder-fixture", &session.render_primary()).unwrap()
        {
            record.apply(&mut session);
        }
        observe_source(&mut session, &format!("new task direction {round}"));
        session.completed_requests += 1;
        let before = session.render_primary();
        let prospective = review_body(&service.config, &session);
        let batch = reminder_ledger(&prospective)["reminder_group_ids"].clone();
        assert!(
            batch
                .as_array()
                .unwrap()
                .iter()
                .all(|v| !dropped.contains(&v.as_u64().unwrap()))
        );
        if round < 2 {
            assert_eq!(batch, first, "a failed review retries its batch");
        }
        let projection = planning::Projection::canonical(&session);
        dispatch_review_for(
            service.clone(),
            id.into(),
            &mut session,
            projection,
            format!("round-{round}"),
        )
        .await;
        let (wire, reply) = tokio::time::timeout(Duration::from_secs(5), captures.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(wire, prospective);
        assert!(
            wire["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == before.last().unwrap()),
            "ordinary new history remains visible beside reminders"
        );
        let pending_body = review_body(&service.config, &session);
        let projection = planning::Projection::canonical(&session);
        dispatch_review_for(
            service.clone(),
            id.into(),
            &mut session,
            projection,
            "pending".into(),
        )
        .await;
        assert!(captures.try_recv().is_err());
        assert_eq!(review_body(&service.config, &session), pending_body);
        // Advance the live owner while advice is in flight. Its source must
        // not retarget the captured batch or get erased at settlement.
        observe_source(&mut session, &format!("arrived during review {round}"));
        save(&service, id, &session).unwrap();
        if round == 0 {
            reply.send(None).unwrap();
        } else {
            let ids = batch.as_array().unwrap();
            let keeps = vec![ids[0].clone()];
            let mut removals = ids[1..].to_vec();
            // Human context is never a reminder, but explicit retirement of an
            // older human group remains supported by the existing owner.
            if round == 1 {
                removals.push(json!(2));
            }
            let pinned = session
                .groups()
                .into_iter()
                .find(|g| !g.replayable)
                .unwrap()
                .id;
            removals.push(json!(pinned)); // must be rejected, not authorized
            reply
                .send(Some(
                    json!({"protected":keeps,"removable":removals,"memories":[]}),
                ))
                .unwrap();
            dropped.extend(ids[1..].iter().map(|v| v.as_u64().unwrap()));
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while service.background_reviews.lock().await.contains(id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        session = load_existing(&service, id).unwrap();
        assert!(!session.background_review_pending);
        assert_eq!(session.shadow.calls, round + 1);
        assert_eq!(session.shadow.input_tokens, round * 100);
        if let Some(record) =
            history::advice(&service, "reminder-fixture", &session.render_primary()).unwrap()
        {
            record.apply(&mut session);
        }
        assert_eq!(
            &session.render_primary()[..old_canonical.len()],
            old_canonical.as_slice()
        );
        assert_eq!(
            session.render_primary().last().unwrap()["content"],
            format!("arrived during review {round}")
        );
        for group in &eligible {
            assert_eq!(session.drop_authorized(group), dropped.contains(&group.id));
        }
        assert!(
            session
                .groups()
                .iter()
                .filter(|g| g.pinned || !g.replayable)
                .all(|g| !session.drop_authorized(g))
        );
        if round > 0 {
            let human = session.groups().into_iter().find(|g| g.id == 2).unwrap();
            assert!(
                session.drop_authorized(&human),
                "explicit human retirement is unchanged"
            );
        }
        let omitted = session
            .groups()
            .iter()
            .filter(|g| session.drop_authorized(g))
            .flat_map(|g| g.members.iter().copied())
            .collect::<HashSet<_>>();
        let selected = session.render_projection(&omitted);
        for group in &eligible {
            for member in &group.members {
                let value = &session
                    .history
                    .iter()
                    .find(|i| i.id == *member)
                    .unwrap()
                    .value;
                assert_eq!(
                    selected.contains(value),
                    !dropped.contains(&group.id),
                    "explicit drops still authorize only complete reviewed cohorts"
                );
            }
        }
    }
    assert!(captures.try_recv().is_err());
    provider.abort();
}
