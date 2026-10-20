// Standalone lease acceptance coverage; no alternative planner or policy.
// Direct stdout is intentional: libtest normally hides passing-test metrics.
fn lease_measurement(value: Value) {
    use std::io::Write;
    writeln!(std::io::stdout(), "LEASE_ACCEPTANCE_MEASUREMENT {value}").unwrap();
}

async fn lease_held_memory_budget_control(budget: &str, grow: bool) {
    let directory = tempfile::tempdir().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let wires = Arc::new(Mutex::new(Vec::<Value>::new()));
    let advice = Arc::new(Mutex::new(
        json!({"protected":[],"removable":[],"memories":[
        {"source_ids":["g2"],"text":"old independently protected fact"}]}),
    ));
    let (seen, answer, signal, gate) = (
        wires.clone(),
        advice.clone(),
        started.clone(),
        release.clone(),
    );
    let router = Router::new()
        .route("/review", post(move |axum::Json(body): axum::Json<Value>| {
            let (seen, answer, signal, gate) =
                (seen.clone(), answer.clone(), signal.clone(), gate.clone());
            async move {
                let n = { let mut seen = seen.lock().await; seen.push(body); seen.len() };
                if n == 3 { signal.notify_one(); gate.notified().await; }
                let advice = answer.lock().await.clone();
                axum::Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":advice.to_string()}]}],"usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":40}}}))
            }
        }))
        .route("/primary", post(|| async {
            axum::Json(json!({"id":"lease-budget-primary","status":"completed","output":[],"usage":{"input_tokens":50,"output_tokens":1,"input_tokens_details":{"cached_tokens":0}}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut service = checkpoint_service(directory.path());
    service.config.background_reviews = true;
    service.config.classifier_url = Some(format!("{base}/review"));
    service.config.upstream_url = format!("{base}/primary");
    service.config.classifier_timeout_secs = 10;
    service.config.classifier_max_output_tokens = 128;
    service.config.min_payback_percent = 100;
    let label = format!("lease-memory-{budget}-{grow}");
    let mut headers = HeaderMap::new();
    headers.insert("x-carry-session", label.parse().unwrap());
    let id = identity(&headers).unwrap().unwrap();
    let mut session = Session::default();
    let input = vec![
        json!({"role":"user","content":"goal"}),
        json!({"role":"assistant","content":"source evidence ".repeat(40)}),
        json!({"role":"user","content":"task"}),
    ];
    session.ingest(&input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    let main = json!({"model":"gpt-6-luna","input":input});
    session.completed_requests = 1; // initial ordinary cadence is genuinely due
    assert!(
        review(
            &service,
            &id,
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    assert_eq!(
        session.memories.len(),
        1,
        "old fact must come from actual HTTP advice"
    );
    let memory_key = format!("m{}", session.memories[0].id);
    *advice.lock().await = json!({"protected":[memory_key.clone()],"removable":[],"memories":[]});
    session.completed_requests += 1;
    assert!(
        review(
            &service,
            &id,
            &mut session,
            &main,
            false,
            &mut ReviewRun::default()
        )
        .await
    );
    session.completed_requests += 8; // only maturity is staged
    let old_lease = session.keep_leases.as_ref().unwrap().entries[&memory_key].clone();
    let proposal = json!({"protected":[],"removable":[],"memories":[
        {"source_ids":["g2"],"text":"new ".repeat(900)}]});
    *advice.lock().await = proposal.clone();
    let mut snapshot = session.clone();
    snapshot.keep_leases.as_mut().unwrap().pending = Some(crate::proxy_state::PendingKeepReview {
        request_id: "budget-probe".into(),
        targets: snapshot.due_keep_targets(),
    });
    assert!(
        snapshot
            .keep_leases
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .targets
            .contains_key(&memory_key)
    );
    let mut snapshot_proposal = snapshot.clone();
    snapshot_proposal
        .apply_advice(&snapshot.groups(), &proposal)
        .unwrap();
    assert_eq!(snapshot_proposal.memories.len(), 2);
    let mut grown_input = input.clone();
    grown_input.push(json!({"role":"user","content":"concurrent task context ".repeat(170)}));
    let mut grown = session.clone();
    grown.ingest(&grown_input).unwrap();
    let mut grown_proposal = grown.clone();
    grown_proposal.memories = snapshot_proposal.memories.clone();
    grown_proposal.memory_anchors = snapshot_proposal.memory_anchors.clone();
    grown_proposal.refresh_memory_shadow();
    let memory_geometry = |s: &Session| {
        let rendered = memory_review_body(&service.config, s);
        let mut empty = s.clone();
        empty.memories.clear();
        empty.active_shadow.retain(|r| r.source_id != 0);
        let base = memory_review_body(&service.config, &empty);
        (
            reviewer_serialized_identity(&rendered).0,
            reviewer_serialized_identity(&base).0,
            estimate(&rendered),
            estimate(&base),
        )
    };
    let (sb, seb, st, set) = memory_geometry(&snapshot_proposal);
    let (gb, geb, gt, get) = memory_geometry(&grown_proposal);
    let mut full_grown = grown.clone();
    full_grown.observe_groups(&full_grown.groups());
    // Derive admission limits from real wire renderings. A 413/setup failure
    // cannot masquerade as current-budget rejection at the merge boundary.
    if budget == "bytes" {
        let lower = (2 * sb - seb)
            .max(gb)
            .max(reviewer_serialized_identity(&review_body(&service.config, &full_grown)).0)
            .max(reviewer_serialized_identity(&review_body(&service.config, &snapshot)).0);
        let upper = 2 * gb - geb;
        assert!(
            lower + 64 < upper,
            "byte fixture needs a nonempty real-render interval: {lower}..{upper}"
        );
        service.config.classifier_max_input_bytes = (lower + 32) as u32;
    } else {
        let lower = (2.0 * st - set)
            .max(reviewer_required_tokens(
                &service.config,
                &memory_review_body(&service.config, &grown_proposal),
            ))
            .max(reviewer_required_tokens(
                &service.config,
                &review_body(&service.config, &full_grown),
            ))
            .max(reviewer_required_tokens(
                &service.config,
                &review_body(&service.config, &snapshot),
            ));
        let upper = 2.0 * gt - get;
        assert!(
            lower.ceil() + 16.0 < upper,
            "token fixture needs a nonempty real-render interval: {lower}..{upper}"
        );
        service.config.classifier_max_input_tokens = (lower.ceil() + 8.0) as u32;
    }
    assert!(review_fits(
        &service.config,
        &review_body(&service.config, &snapshot)
    ));
    assert!(review_fits(
        &service.config,
        &review_body(&service.config, &full_grown)
    ));
    assert!(memory_admission_fits(&service.config, &snapshot_proposal));
    assert!(memory_admission_fits(&service.config, &grown));
    assert!(
        review_fits(
            &service.config,
            &memory_review_body(&service.config, &grown_proposal)
        ),
        "proposed memory fits hard capacity: only the half-capacity reserve may reject it"
    );
    assert!(!memory_admission_fits(&service.config, &grown_proposal));
    let mut accepted_snapshot = snapshot.clone();
    accepted_snapshot
        .apply_advice_with_memory_admission(&snapshot.groups(), &proposal, |candidate| {
            memory_admission_fits(&service.config, candidate)
        })
        .unwrap();
    assert_eq!(accepted_snapshot.review_feedback["rejected_count"], 0);
    save(&service, &id, &session).unwrap();
    let service = Arc::new(service);
    dispatch_review_for(
        service.clone(),
        id.clone(),
        &mut session,
        main.clone(),
        "held-memory-budget".into(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    let actual_wire = wires.lock().await[2].clone();
    assert!(review_fits(&service.config, &actual_wire));
    let ledger: Value = serde_json::from_str(
        actual_wire["input"].as_array().unwrap().last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(
        ledger["keep_lease_review"]["target_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(memory_key)),
        "actual dispatched target must name the protected memory"
    );
    let following = json!({"model":"gpt-6-luna","input":if grow {grown_input} else {input}});
    assert!(primary_fits(&service.config, &following));
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        forward_request(
            service.clone(),
            headers,
            serde_json::to_vec(&following).unwrap().into(),
            false,
            "while-memory-held".into(),
        ),
    )
    .await
    .expect("fitting primary must complete while target reviewer is held")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "413 is not semantic RED");
    axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let current = load(&service, &id).unwrap();
    assert!(current.background_review_pending);
    assert_eq!(current.primary.calls, 1);
    assert_eq!(current.shadow.calls, 2);
    assert_eq!(
        current.keep_leases.as_ref().unwrap().entries[&memory_key],
        old_lease
    );
    let current_geometry = memory_admission_fits(&service.config, &{
        let mut candidate = current.clone();
        candidate.keep_leases.as_mut().unwrap().pending = None;
        candidate.memories = snapshot_proposal.memories.clone();
        candidate.memory_anchors = snapshot_proposal.memory_anchors.clone();
        candidate.refresh_memory_shadow();
        candidate
    });
    assert_eq!(current_geometry, !grow);
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.background_reviews.lock().await.contains(&id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let merged = load(&service, &id).unwrap();
    assert!(!merged.background_review_pending);
    assert!(merged.keep_leases.as_ref().unwrap().pending.is_none());
    assert_eq!(merged.shadow.calls, 3);
    assert_eq!(
        (
            merged.shadow.input_tokens,
            merged.shadow.output_tokens,
            merged.shadow.cached_tokens
        ),
        (300, 30, 120)
    );
    assert_eq!(merged.shadow.unavailable_cost_calls, 0);
    let rates = Rates::for_model(&service.config.classifier_model, 100.0).unwrap();
    let receipt = 60.0 * rates.input + 40.0 * rates.cached + 10.0 * rates.output;
    assert!((merged.shadow.cost_usd - 3.0 * receipt).abs() < 1e-12);
    assert_eq!(merged.primary.calls, 1);
    assert_eq!(merged.completed_requests, current.completed_requests);
    assert_eq!(merged.history_rebases, session.history_rebases);
    assert_eq!(
        merged.review_feedback["rejected_count"],
        if grow { 1 } else { 0 }
    );
    assert_eq!(merged.memories.len(), if grow { 1 } else { 2 });
    assert_eq!(
        merged.keep_leases.as_ref().unwrap().entries[&memory_key]
            .released_at_round
            .is_some(),
        !grow,
        "rejected current-budget memory must not turn valid-looking target omission into release"
    );
    if grow {
        assert_eq!(
            merged.keep_leases.as_ref().unwrap().entries[&memory_key],
            old_lease
        );
        assert_eq!(merged.review_feedback["accepted_memories"], 0);
        assert_eq!(
            merged.review_feedback["rejected"][0]["field"], "memories",
            "must reject committed memory admission, not discard a stale batch"
        );
        assert!(
            merged.review_feedback["rejected"][0]["reason"]
                .as_str()
                .unwrap()
                .starts_with(
                    "background memory rejected: current aggregate byte or token budget exceeded"
                )
        );
        assert_eq!(merged.partial_reviews, current.partial_reviews + 1);
    }
    let events = pr154_journal(directory.path(), &id);
    assert!(
        events
            .iter()
            .any(|entry| entry["event"] == "background_review_merged"),
        "current-budget case must reach compatible background merge, not stale discard"
    );
    let mut restarted = checkpoint_service(directory.path());
    restarted.config.classifier_max_input_bytes = service.config.classifier_max_input_bytes;
    restarted.config.classifier_max_input_tokens = service.config.classifier_max_input_tokens;
    restarted.config.classifier_max_output_tokens = service.config.classifier_max_output_tokens;
    restarted.config.background_reviews = true;
    drop(service);
    let mut resumed = load(&restarted, &id).unwrap();
    resumed.initialize_keep_leases(restarted.config.keep_lease_turns);
    assert_eq!(
        serde_json::to_value(&resumed).unwrap(),
        serde_json::to_value(&merged).unwrap()
    );
    assert_eq!(
        load(&restarted, &id).unwrap().shadow.calls,
        3,
        "restart/readback cannot replay paid receipt"
    );
    assert_eq!(
        wires.lock().await.len(),
        3,
        "no rescue or hidden fourth review"
    );
    lease_measurement(
        json!({"scenario":"held_target_memory_current_budget","budget":budget,"grow":grow,
        "snapshot_memory_bytes":sb,"snapshot_empty_bytes":seb,"grown_memory_bytes":gb,"grown_empty_bytes":geb,
        "snapshot_memory_tokens":st,"snapshot_empty_tokens":set,"grown_memory_tokens":gt,"grown_empty_tokens":get,
        "byte_limit":restarted.config.classifier_max_input_bytes,"token_limit":restarted.config.classifier_max_input_tokens,
        "actual_dispatch_bytes":reviewer_serialized_identity(&actual_wire).0,"actual_dispatch_tokens":estimate(&actual_wire),
        "paid_calls":resumed.shadow.calls,"paid_usd":resumed.shadow.cost_usd,"rejected":resumed.review_feedback["rejected_count"],
        "released":resumed.keep_leases.as_ref().unwrap().entries[&memory_key].released_at_round.is_some()}),
    );
    provider.abort();
}

#[tokio::test]
async fn lease_acceptance_held_memory_byte_rejection_preserves_protection() {
    lease_held_memory_budget_control("bytes", true).await;
}
#[tokio::test]
async fn lease_acceptance_held_memory_byte_unchanged_positive_control() {
    lease_held_memory_budget_control("bytes", false).await;
}
#[tokio::test]
async fn lease_acceptance_held_memory_token_rejection_preserves_protection() {
    lease_held_memory_budget_control("tokens", true).await;
}
#[tokio::test]
async fn lease_acceptance_held_memory_token_unchanged_positive_control() {
    lease_held_memory_budget_control("tokens", false).await;
}

fn lease_paid_target(config: &ProxyCli, session: &Session) -> (Session, f64, Value) {
    let mut released = session.clone();
    let pending = crate::proxy_state::PendingKeepReview {
        request_id: "measured-target-acquisition".into(),
        targets: session.due_keep_targets(),
    };
    assert!(!pending.targets.is_empty());
    released.keep_leases.as_mut().unwrap().pending = Some(pending.clone());
    let wire = review_body(config, &released);
    assert!(review_fits(config, &wire));
    let paid = reviewer_view_costs(config, &session.reviewer_cache, &wire, &wire, 1)
        .unwrap()
        .keep
        + config.classifier_max_output_tokens as f64
            * Rates::for_model(&config.classifier_model, estimate(&wire))
                .unwrap()
                .output;
    released.resolve_keep_targets(&pending);
    released.keep_leases.as_mut().unwrap().pending = None;
    (released, paid, wire)
}

#[test]
fn lease_acceptance_due_reviewer_cache_loss_reverses_shared_candidate_gain() {
    let mut config = reviewer_config();
    config.classifier_model = "gpt-6-sol".into();
    config.min_payback_percent = 0;
    let mut session = Session::default();
    let input = vec![
        json!({"role":"user","content":"durable exact context ".repeat(1600)}),
        json!({"type":"function_call","call_id":"due","name":"native","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"due","output":"disposable exact evidence ".repeat(1600)}),
        json!({"role":"user","content":"current task"}),
    ];
    session.ingest(&input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.initialize_keep_leases(8);
    let groups = session.groups();
    assert_eq!(groups[1].members.len(), 2, "actual complete native cohort");
    session
        .apply_advice(
            &groups,
            &json!({"protected":["g1","g2"],"removable":[],"memories":[]}),
        )
        .unwrap();
    session.observe_groups(&groups);
    admission::cover(&mut session, &groups);
    session.completed_requests = 8;
    session.last_review_request = 8; // accepted ordinary target review is sunk
    let main = json!({"model":"gpt-6-luna","input":session.render_primary()});
    session.review_context = cache_base(&main);
    session.review_cache_key = "lease-cache-counterexample".into();
    let (_, _, targeted) = lease_paid_target(&config, &session);
    // Real production cache observation boundary: requested write, then integer
    // native hit receipt. No synthetic ReviewerCacheBoundary credit is injected.
    for cached in [0, 10000] {
        observe_reviewer_cache(
            &config,
            &mut session.reviewer_cache,
            &targeted,
            &json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":cached}}),
        );
    }
    let (released, paid, targeted) = lease_paid_target(&config, &session);
    assert!(reviewer_reusable(&config, &session.reviewer_cache, &targeted) > 0.0);
    let (candidate, report) = plan(&config, &released, &main);
    let row = report["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["removed_groups"] == json!([2]))
        .unwrap();
    assert_eq!(row["feasible"], true);
    assert_eq!(row["future_reviewer_keep_calls"], 4);
    assert_eq!(row["future_reviewer_compact_calls"], 4);
    let joint = &row["joint"];
    let primary_gain =
        joint["primary_keep"].as_f64().unwrap() - joint["primary_compact"].as_f64().unwrap();
    let reviewer_loss = joint["future_shadow_compact"].as_f64().unwrap()
        - joint["future_shadow_keep"].as_f64().unwrap();
    assert!(
        primary_gain > 0.0,
        "primary-only apparent gain must be real: {report}"
    );
    assert!(
        reviewer_loss > primary_gain,
        "reviewer-prefix loss must reverse it: {report}"
    );
    assert_eq!(joint["accepted"], false);
    assert!(candidate.is_none());
    assert!(
        !planning::economic_candidate(&config, &released, &main, 0.0),
        "actual shared candidate must veto primary gain after the ordinary target review is sunk"
    );
    let mut removed = released.clone();
    removed.remove(&[2], &removed.groups());
    assert_eq!(
        reviewer_reusable(
            &config,
            &removed.reviewer_cache,
            &review_body(&config, &removed)
        ),
        0.0
    );
    let mut cold = session.clone();
    cold.reviewer_cache.clear();
    let (cold_released, cold_paid, _) = lease_paid_target(&config, &cold);
    assert!(
        planning::economic_candidate(&config, &cold_released, &main, 0.0),
        "same due release with only reviewer cache evidence removed must be useful"
    );
    lease_measurement(
        json!({"scenario":"due_target_reviewer_cache_monetary_reversal",
        "primary_gain_usd":primary_gain,"reviewer_loss_usd":reviewer_loss,
        "cached_acquisition_usd":paid,"cold_acquisition_usd":cold_paid,
        "cached_candidate_accepted":false,"cold_candidate_accepted":true,"ordinary_target_review_is_sunk":true,
        "targeted_tokens":estimate(&targeted),"cache_credit_tokens":reviewer_reusable(&config,&session.reviewer_cache,&targeted),
        "actual_candidate":row}),
    );

    // Mixed eligible/due controls use the identical actual admission/planner.
    // A useful already-authorized suffix wins without purchasing due advice.
    let mut mixed = session.clone();
    mixed.completed_requests += 1;
    let mut extended = input.clone();
    extended.extend([
        json!({"role":"assistant","content":"eligible suffix ".repeat(1000)}),
        json!({"role":"user","content":"next task"}),
    ]);
    mixed.ingest(&extended).unwrap();
    for item in &mut mixed.history {
        item.exposed = true;
    }
    let groups = mixed.groups();
    let suffix = groups.iter().find(|g| g.id > 4 && !g.human).unwrap().id;
    mixed
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[format!("g{suffix}")],"memories":[]}),
        )
        .unwrap();
    mixed.observe_groups(&groups);
    admission::cover(&mut mixed, &groups);
    config.review_min_new_tokens = 25000;
    let mixed_main = json!({"model":"gpt-6-sol","input":mixed.render_primary()});
    assert!(mixed.due_keep_targets().contains_key("g2"));
    assert!(planning::economic_candidate(
        &config,
        &mixed,
        &mixed_main,
        0.0
    ));
    assert_eq!(
        admission::decision(&config, &mixed, &mixed_main, false)["admitted"],
        false
    );

    // An eligible but below-payback tiny suffix must not mask useful due work.
    let mut tiny = session.clone();
    tiny.completed_requests += 1;
    let mut extended = input;
    extended.extend([
        json!({"role":"assistant","content":"tiny"}),
        json!({"role":"user","content":"next task"}),
    ]);
    tiny.ingest(&extended).unwrap();
    for item in &mut tiny.history {
        item.exposed = true;
    }
    let groups = tiny.groups();
    tiny.apply_advice(
        &groups,
        &json!({"protected":[],"removable":[format!("g{suffix}")],"memories":[]}),
    )
    .unwrap();
    tiny.observe_groups(&groups);
    admission::cover(&mut tiny, &groups);
    config.min_payback_percent = 10;
    let tiny_main = json!({"model":"gpt-6-astra","input":tiny.render_primary()});
    assert!(!planning::economic_candidate(
        &config, &tiny, &tiny_main, 0.0
    ));
    let (tiny_release, tiny_paid, _) = lease_paid_target(&config, &tiny);
    assert!(planning::economic_candidate(
        &config,
        &tiny_release,
        &tiny_main,
        tiny_paid
    ));
    assert_eq!(
        admission::decision(&config, &tiny, &tiny_main, false)["reason"],
        "keep_lease_economic"
    );
    lease_measurement(
        json!({"scenario":"mixed_existing_eligible_and_due_controls",
        "eligible_suffix":suffix,"sufficient_existing_candidate_no_extra":true,
        "tiny_existing_candidate":false,"tiny_plus_due_candidate":true,"tiny_acquisition_usd":tiny_paid}),
    );
}

fn lease_many_cohorts(count: usize, payload_repetitions: usize) -> Session {
    let mut input = vec![json!({"role":"user","content":"goal"})];
    for index in 0..count {
        input.extend([
            json!({"type":"function_call","call_id":format!("cohort-{index}"),"name":"native","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":format!("cohort-{index}"),"output":"large synthetic evidence ".repeat(payload_repetitions)}),
            json!({"role":"user","content":format!("next-{index}")}),
        ]);
    }
    let mut session = Session::default();
    session.ingest(&input).unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.initialize_keep_leases(8);
    let groups = session.groups();
    let protected = groups
        .iter()
        .filter(|g| !g.human)
        .map(|g| format!("g{}", g.id))
        .collect::<Vec<_>>();
    assert_eq!(protected.len(), count);
    assert!(
        groups
            .iter()
            .filter(|g| !g.human)
            .all(|g| g.members.len() == 2 && !g.pinned)
    );
    session
        .apply_advice(
            &groups,
            &json!({"protected":protected,"removable":[],"memories":[]}),
        )
        .unwrap();
    assert_eq!(session.review_feedback["rejected_count"], 0);
    session.completed_requests = 8;
    session
}

#[test]
fn lease_acceptance_serialized_metadata_scaling_pruning_retirement_restart() {
    let started = std::time::Instant::now();
    let mut samples = Vec::new();
    for count in [8, 32, 64] {
        let mut session = lease_many_cohorts(count, 200);
        let metadata = serde_json::to_vec(&session.keep_leases).unwrap();
        let members = session
            .keep_leases
            .as_ref()
            .unwrap()
            .entries
            .values()
            .map(|l| l.members.len())
            .sum::<usize>();
        assert_eq!(members, count * 2);
        // Bounded to this fixture's numeric widths/membership, not a universal
        // byte limit. Metadata contains digests/IDs, never copied raw payloads.
        assert!(metadata.len() <= 256 + count * 320);
        let larger_payload = lease_many_cohorts(count, 400);
        assert_eq!(
            metadata.len(),
            serde_json::to_vec(&larger_payload.keep_leases)
                .unwrap()
                .len(),
            "doubling raw bytes must not duplicate them into lease metadata"
        );
        let raw_bytes = serde_json::to_vec(&session.history).unwrap().len();
        assert_eq!(session.due_keep_target_count(), count);
        let targets = session.due_keep_targets();
        assert_eq!(targets.len(), 4);
        session.reserve_extra_keep_attempt();
        session.keep_leases.as_mut().unwrap().pending =
            Some(crate::proxy_state::PendingKeepReview {
                request_id: "measured-held-targets".into(),
                targets: targets.clone(),
            });
        let pending_bytes = serde_json::to_vec(&session.keep_leases).unwrap().len();
        assert!(pending_bytes <= metadata.len() + 4 * 384 + 256);
        let directory = tempfile::tempdir().unwrap();
        let service = checkpoint_service(directory.path());
        save(&service, "lease-state-scaling", &session).unwrap();
        let mut resumed = load(&service, "lease-state-scaling").unwrap();
        assert_eq!(
            serde_json::to_value(&resumed).unwrap(),
            serde_json::to_value(&session).unwrap()
        );
        let pending = resumed
            .keep_leases
            .as_mut()
            .unwrap()
            .pending
            .take()
            .unwrap();
        resumed.resolve_keep_targets(&pending);
        let remove = resumed
            .groups()
            .iter()
            .filter(|g| resumed.drop_authorized(g))
            .map(|g| g.id)
            .collect::<Vec<_>>();
        assert_eq!(remove.len(), 4);
        resumed.remove(&remove, &resumed.groups());
        resumed.initialize_keep_leases(8);
        assert_eq!(
            resumed.keep_leases.as_ref().unwrap().entries.len(),
            count - 4
        );
        assert!(
            resumed
                .keep_leases
                .as_ref()
                .unwrap()
                .extra_attempted
                .is_empty()
        );
        let pruned_bytes = serde_json::to_vec(&resumed.keep_leases).unwrap().len();
        assert!(pruned_bytes < metadata.len());
        for key in targets.keys() {
            assert!(
                !resumed
                    .keep_leases
                    .as_ref()
                    .unwrap()
                    .entries
                    .contains_key(key)
            );
        }
        let removed_members = session
            .groups()
            .iter()
            .filter(|g| remove.contains(&g.id))
            .flat_map(|g| g.members.iter().copied())
            .collect::<HashSet<_>>();
        for item in &resumed.history {
            assert_eq!(item.removed, removed_members.contains(&item.id));
        }
        save(&service, "lease-state-scaling", &resumed).unwrap();
        assert_eq!(
            serde_json::to_value(load(&service, "lease-state-scaling").unwrap()).unwrap(),
            serde_json::to_value(&resumed).unwrap()
        );
        samples.push(
            json!({"cohorts":count,"members":members,"raw_history_bytes":raw_bytes,
            "lease_bytes":metadata.len(),"pending_lease_bytes":pending_bytes,
            "pruned_lease_bytes":pruned_bytes,"remaining_entries":count-4}),
        );
    }
    // Independent fact retirement exercises the physical path, both maps, and
    // checkpoint restart; no duplicated raw cohort resurrection is permitted.
    let mut facts = lease_many_cohorts(8, 20);
    let groups = facts.groups();
    let source = groups.iter().find(|g| !g.human).unwrap().id;
    facts
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[],"memories":[
        {"source_ids":[format!("g{source}")],"text":"retire this independently protected fact ".repeat(80)},
        {"source_ids":[format!("g{source}")],"text":"preserve sibling fact"}]}),
        )
        .unwrap();
    let keys = facts
        .memories
        .iter()
        .map(|m| format!("m{}", m.id))
        .collect::<Vec<_>>();
    facts
        .apply_advice(
            &groups,
            &json!({"protected":keys,"removable":[],"memories":[]}),
        )
        .unwrap();
    facts.completed_requests += 8;
    facts.reserve_extra_keep_attempt();
    // Stage the already-due memory's accepted target resolution. This is a
    // state/pruning component, not another claim of a live HTTP review.
    let memory_key = keys[0].clone();
    let lease = facts.keep_leases.as_ref().unwrap().entries[&memory_key].clone();
    facts.resolve_keep_targets(&crate::proxy_state::PendingKeepReview {
        request_id: "state-memory-retirement".into(),
        targets: [(memory_key.clone(), lease)].into_iter().collect(),
    });
    assert!(
        facts
            .keep_leases
            .as_ref()
            .unwrap()
            .extra_attempted
            .contains_key(&memory_key)
    );
    let before_fact_bytes = serde_json::to_vec(&facts.keep_leases).unwrap().len();
    facts.remove(&[], &groups);
    facts.initialize_keep_leases(8);
    assert_eq!(facts.memories.len(), 1);
    assert_eq!(facts.memories[0].text, "preserve sibling fact");
    assert!(
        !facts
            .keep_leases
            .as_ref()
            .unwrap()
            .entries
            .contains_key(&memory_key)
    );
    assert!(
        !facts
            .keep_leases
            .as_ref()
            .unwrap()
            .extra_attempted
            .contains_key(&memory_key)
    );
    let after_fact_bytes = serde_json::to_vec(&facts.keep_leases).unwrap().len();
    assert!(after_fact_bytes < before_fact_bytes);
    let directory = tempfile::tempdir().unwrap();
    let service = checkpoint_service(directory.path());
    save(&service, "lease-fact-pruning", &facts).unwrap();
    let restarted = load(&service, "lease-fact-pruning").unwrap();
    assert_eq!(
        serde_json::to_value(&restarted).unwrap(),
        serde_json::to_value(&facts).unwrap()
    );
    assert_eq!(restarted.released_memory_ids().len(), 0);
    lease_measurement(
        json!({"scenario":"serialized_lease_state_scaling_pruning_restart",
        "samples":samples,"fact_retirement_before_bytes":before_fact_bytes,"fact_retirement_after_bytes":after_fact_bytes,
        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "claim":"scenario-specific measurements and structural bounds, not worst-case guarantee"}),
    );
}

#[test]
fn lease_acceptance_max_horizon_cache_frontiers_bound_distinct_action_work() {
    let started = std::time::Instant::now();
    let mut config = reviewer_config();
    config.payoff_requests = 100; // actual public maximum, not a new policy
    config.min_payback_percent = 0;
    // Ordinary growth threshold disabled in this scenario: exercise up to 100
    // actual dispatch projections without repeatedly nesting extra-admission probes.
    config.review_min_new_tokens = 0;
    let mut session = lease_many_cohorts(12, 8);
    let groups = session.groups();
    let due = session.due_keep_targets();
    assert_eq!(due.len(), 4);
    // Existing eligible and still-due cohorts coexist. Keep target generations
    // active while constructing eight compatible actual primary cache frontiers.
    let eligible = groups
        .iter()
        .filter(|g| !g.human && !due.contains_key(&format!("g{}", g.id)))
        .map(|g| format!("g{}", g.id))
        .collect::<Vec<_>>();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":eligible,"memories":[]}),
        )
        .unwrap();
    let groups = session.groups();
    session.observe_groups(&groups);
    admission::cover(&mut session, &groups);
    let body = json!({"model":"gpt-6-sol","input":session.render_primary()});
    for length in [4, 7, 10, 13, 16, 19, 22, 25, 28, 31] {
        let mut prefix = body.clone();
        prefix["input"].as_array_mut().unwrap().truncate(length);
        observe_cache(
            &mut session.primary_cache,
            &prefix,
            &json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":100}}),
        );
    }
    assert_eq!(session.primary_cache.len(), 8);
    let (candidate, report) = plan(&config, &session, &body);
    let actions = report["candidates"].as_array().unwrap();
    let removed = actions
        .iter()
        .map(|row| serde_json::from_value::<Vec<u64>>(row["removed_groups"].clone()).unwrap())
        .collect::<Vec<_>>();
    let unique = removed.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        removed.len(),
        "one expensive forecast per distinct removal action"
    );
    assert!(
        removed.len() >= 5 && removed.len() <= session.primary_cache.len() + 2,
        "eight cache frontiers plus empty/all-protected probes bound distinct action work: {report}"
    );
    assert!(candidate.is_some());
    for action in &removed {
        assert!(
            action.iter().all(|id| !due.contains_key(&format!("g{id}"))),
            "still-due protection cannot be retired"
        );
    }
    for row in actions {
        for field in [
            "future_reviewer_keep_calls",
            "future_reviewer_compact_calls",
        ] {
            assert!(row[field].as_u64().is_some_and(|n| n <= 100));
        }
    }
    lease_measurement(json!({"scenario":"max_horizon_cache_frontier_action_work",
        "horizon":config.payoff_requests,"cache_frontiers":session.primary_cache.len(),"due_targets":due.len(),
        "distinct_evaluated_actions":removed.len(),"actions":removed,
        "structural_action_upper_bound":session.primary_cache.len()+2,
        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "claim":"one bounded scenario; no worst-case latency guarantee; HTTP deadline remains 30 seconds"}));
}

fn lease_tier_fixture(retained_repetitions: usize) -> Session {
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"durable ".repeat(retained_repetitions)}),
            json!({"role":"assistant","content":"evidence ".repeat(45000)}),
            json!({"role":"user","content":"finish"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.initialize_keep_leases(8);
    let groups = session.groups();
    session
        .apply_advice(
            &groups,
            &json!({"protected":["g1","g2"],"removable":[],"memories":[]}),
        )
        .unwrap();
    session.observe_groups(&groups);
    admission::cover(&mut session, &groups);
    session.review_cache_key = "lease-target-tier".into();
    session.completed_requests = 8;
    session
}

#[test]
fn lease_acceptance_target_tail_tier_crossing_reverses_shared_candidate_gain() {
    let mut config = reviewer_config();
    config.classifier_model = "gpt-6-sol".into();
    config.classifier_max_input_tokens = 2_000_000;
    config.primary_context_tokens = 2_000_000;
    config.review_min_new_tokens = 25000;
    config.min_payback_percent = 0;
    let threshold = carry::models::model_spec(&config.classifier_model)
        .unwrap()
        .long_context
        .unwrap()
        .threshold;
    let seed = lease_tier_fixture(1);
    let ordinary_seed = estimate(&review_body(&config, &seed));
    let (_, _, targeted_seed) = lease_paid_target(&config, &seed);
    let annotation = estimate(&targeted_seed) - ordinary_seed;
    assert!(annotation > 2.0);
    // A bounded fixture calibration using actual render/tokenizer results, not
    // manually priced primary/reviewer requests or a handwritten planner.
    let mut repetitions = (threshold - annotation / 2.0 - ordinary_seed + 1.0) as usize;
    let mut session = lease_tier_fixture(repetitions);
    for _ in 0..4 {
        let ordinary = estimate(&review_body(&config, &session));
        let correction = (threshold - annotation / 2.0 - ordinary).round() as i64;
        if correction == 0 {
            break;
        }
        repetitions = (repetitions as i64 + correction) as usize;
        session = lease_tier_fixture(repetitions);
    }
    let ordinary = review_body(&config, &session);
    let (_, _, targeted) = lease_paid_target(&config, &session);
    assert!(
        estimate(&ordinary) <= threshold && estimate(&targeted) > threshold,
        "only the actual due-target ledger must cross the tariff: {} / {} / {threshold}",
        estimate(&ordinary),
        estimate(&targeted)
    );
    for cached in [0, 1_000_000] {
        observe_reviewer_cache(
            &config,
            &mut session.reviewer_cache,
            &targeted,
            &json!({"input_tokens":1_000_000,"input_tokens_details":{"cached_tokens":cached}}),
        );
    }
    let (released, paid, targeted) = lease_paid_target(&config, &session);
    let main = json!({"model":"gpt-5.6-terra","input":session.render_primary()});
    assert!(primary_fits(&config, &main));
    let high = Rates::for_model(&config.classifier_model, estimate(&targeted)).unwrap();
    let low = Rates::for_model(&config.classifier_model, estimate(&ordinary)).unwrap();
    assert_eq!(high.input, 2.0 * low.input);
    assert!((high.output - 1.5 * low.output).abs() < 1e-18);
    let actual_input_cost =
        reviewer_view_costs(&config, &session.reviewer_cache, &targeted, &targeted, 1)
            .unwrap()
            .keep;
    let underpriced =
        actual_input_cost / 2.0 + config.classifier_max_output_tokens as f64 * low.output;
    assert!(
        planning::economic_candidate(&config, &released, &main, 0.0),
        "primary-only release really appears useful"
    );
    assert!(
        planning::economic_candidate(&config, &released, &main, underpriced),
        "same actual shared candidate accepts if acquisition is incorrectly priced below the boundary"
    );
    assert!(
        !planning::economic_candidate(&config, &released, &main, paid),
        "actual targeted request's higher input/output tier must reverse that apparent gain"
    );
    assert_eq!(
        admission::decision(&config, &session, &main, false)["admitted"],
        false
    );
    lease_measurement(json!({"scenario":"due_target_tail_tier_monetary_reversal",
        "ordinary_tokens":estimate(&ordinary),"targeted_tokens":estimate(&targeted),"tier_threshold":threshold,
        "actual_paid_usd":paid,"underpriced_control_usd":underpriced,
        "primary_only_candidate":true,"underpriced_candidate":true,"actual_candidate":false,
        "cache_credit_tokens":reviewer_reusable(&config,&session.reviewer_cache,&targeted)}));
}
