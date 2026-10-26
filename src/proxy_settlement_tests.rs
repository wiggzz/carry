// Test-only process-exit/fault seams. The release executable has neither hook.
pub(super) fn publication_crash_barrier(service: &Service, stage: &str) {
    if std::env::var_os("CARRY_TEST_PUBLICATION_ROOT").as_deref()
        == Some(service.config.state_dir.as_os_str())
    {
        let action = std::env::var("CARRY_TEST_PUBLICATION_STAGE").unwrap();
        eprintln!("completed-H crash hook: action={action}, stage={stage}");
        if action == stage {
            std::process::exit(73);
        }
        if action == "fault" && stage == "before" {
            std::fs::create_dir(service.config.state_dir.join(".history-index.tmp")).unwrap();
        }
    }
}

#[tokio::test]
async fn paid_publication_crash_child() {
    let Some(root) = std::env::var_os("CARRY_TEST_PUBLICATION_ROOT") else {
        return;
    };
    let mut service = checkpoint_service(std::path::Path::new(&root));
    service.config.classifier_url = Some(std::env::var("CARRY_TEST_PUBLICATION_PROVIDER").unwrap());
    let service = Arc::new(service);
    let mut session = semantic_subject();
    session.history_shared = true;
    session.bootstrap_review_pending = true;
    let projection = planning::Projection::canonical(&session);
    eprintln!("completed-H child: dispatching actual reviewer");
    dispatch_review_for(
        service.clone(),
        "crash-owner".into(),
        &mut session,
        projection,
        "crash-boundary".into(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while service
            .background_reviews
            .lock()
            .await
            .contains("crash-owner")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn completed_h_crash_before_after_publication_and_failed_publication_keep_known_receipt_once()
{
    let (url, mut captures, provider) = review_capture_provider().await;
    for stage in ["before", "after", "fault"] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_owned();
        let endpoint = url.clone();
        let output = tokio::task::spawn_blocking(move || {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "proxy::tests::paid_publication_crash_child",
                    "--nocapture",
                ])
                .env("CARRY_TEST_PUBLICATION_ROOT", root)
                .env("CARRY_TEST_PUBLICATION_PROVIDER", endpoint)
                .env("CARRY_TEST_PUBLICATION_STAGE", stage)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            child.wait_with_output().unwrap()
        })
        .await
        .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if stage == "fault" { 0 } else { 73 }),
            "{output:?}"
        );
        tokio::time::timeout(Duration::from_secs(10), captures.recv())
            .await
            .expect("reviewer transport capture deadline")
            .unwrap();
        let mut service = checkpoint_service(directory.path());
        service.config.classifier_url = Some(url.clone());
        service.config.review_min_primary_tokens = 0;
        service.config.review_min_new_tokens = 0;
        service.config.review_backoff_factor = 1;
        let durable = history::restore(&service, "crash-owner").unwrap();
        assert_eq!(durable.shadow.calls, 1);
        assert_eq!(durable.shadow.input_tokens, 10000);
        assert_eq!(durable.shadow.output_tokens, 7);
        assert!(!durable.background_review_pending);
        assert_eq!(
            durable.bootstrap_review_pending,
            stage != "after",
            "receipt settlement cannot consume bootstrap before durable semantic publication"
        );
        let h = semantic_subject().render_primary();
        assert_eq!(
            history::advice(&service, "fixture-owner", &h)
                .unwrap()
                .is_some(),
            stage == "after"
        );
        if stage == "fault" {
            std::fs::remove_dir(directory.path().join(".history-index.tmp")).unwrap();
        }
        // A new owner can reserve the entire one-owner budget: failed publication
        // must not leak its worker pin. For crashes this also proves cold restart.
        service.config.history_max_references = 1;
        drop(history::pin(&service, "new-owner").unwrap());
        service.config.history_max_references = 10000;
        let service = Arc::new(service);
        let mut restored = load_existing(&service, "crash-owner").unwrap();
        // A settled owner with no new turn is deliberately not due. Advance the
        // ordinary request cadence before retrying this same original H.
        restored.completed_requests += service.config.review_every_requests;
        let projection = planning::Projection::canonical(&restored);
        dispatch_review_for(
            service.clone(),
            "crash-owner".into(),
            &mut restored,
            projection,
            "retry".into(),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), captures.recv())
            .await
            .expect("reviewer transport capture deadline")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while service
                .background_reviews
                .lock()
                .await
                .contains("crash-owner")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let durable = history::restore(&service, "crash-owner").unwrap();
        assert_eq!(
            durable.shadow.calls, 2,
            "two actual paid requests, neither lost nor duplicated"
        );
        assert_eq!(durable.shadow.input_tokens, 20000);
        assert!(
            history::advice(&service, "fixture-owner", &h)
                .unwrap()
                .is_some()
        );
    }
    provider.abort();
}

#[tokio::test]
async fn failed_known_receipt_settlement_releases_live_owner_pin_without_publishing_h() {
    let (url, mut captures, provider) = review_capture_provider().await;
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.classifier_url = Some(url);
    service.config.history_max_references = 1;
    let service = Arc::new(service);
    let id = "failed-receipt-owner";
    let mut session = semantic_subject();
    session.history_shared = true;
    session.bootstrap_review_pending = true;
    let h = session.render_primary();
    let lock = session_lock(&service, id).await;
    let (sender, mut waits) = tokio::sync::mpsc::unbounded_channel();
    *service.lock_waits.lock().unwrap() = Some(sender);
    let projection = planning::Projection::canonical(&session);
    dispatch_review_for(
        service.clone(),
        id.into(),
        &mut session,
        projection,
        "failed-receipt".into(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), captures.recv())
        .await
        .expect("reviewer transport capture deadline")
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), waits.recv())
            .await
            .expect("paid owner settlement wait deadline")
            .unwrap(),
        id
    );
    let fault = directory.path().join(".history-index.tmp");
    std::fs::create_dir(&fault).unwrap();
    drop(lock);
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.background_reviews.lock().await.contains(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let durable = history::restore(&service, id).unwrap();
    assert!(
        durable.background_review_pending,
        "known but uncommitted attempt remains explicitly unknown for restart censorship"
    );
    assert_eq!(durable.shadow.calls, 0);
    assert!(
        history::advice(&service, "fixture-owner", &h)
            .unwrap()
            .is_none()
    );
    drop(history::pin(&service, "other-live-owner").unwrap());
    std::fs::remove_dir(fault).unwrap();
    provider.abort();
}

#[test]
fn cumulative_memory_acceptances_enforce_reserve_and_hard_token_limits_then_retire_one_same_source_fact()
 {
    let mut config = reviewer_config();
    config.classifier_cache_policy = ClassifierCachePolicy::Disabled;
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"goal"}),
            json!({"role":"assistant","content":"evidence"}),
            json!({"role":"user","content":"task"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    let add = |state: &mut Session, n| {
        state.apply_advice_with_memory_admission(&state.groups(), &json!({"protected":[],"removable":[],"memories":[{"source_ids":[2],"text":format!("fact-{n}:{}", "x".repeat(3980))}]}), |candidate| memory_admission_fits(&config,candidate,&HashSet::new())).unwrap();
    };
    for n in 0..4 {
        add(&mut session, n);
        assert_eq!(session.memories.len(), n + 1);
    }
    let prior = session.clone();
    let mut candidate = session.clone();
    add(&mut candidate, 4);
    assert_eq!(candidate.memories.len(), 5);
    let bytes = serde_json::to_vec(&candidate.memories).unwrap().len();
    config.classifier_max_input_bytes = (bytes * 2 - 2) as u32;
    assert!(
        review_fits(&config, &review_body(&config, &candidate)),
        "full transport still fits while half-capacity memory reserve rejects cumulative growth"
    );
    assert!(!memory_admission_fits(&config, &candidate, &HashSet::new()));
    let mut bounded = prior.clone();
    bounded.apply_advice_with_memory_admission(&bounded.groups(), &json!({"protected":[],"removable":[],"memories":[{"source_ids":[2],"text":candidate.memories[4].text}]}), |s| memory_admission_fits(&config,s,&HashSet::new())).unwrap();
    assert_eq!(bounded.memories.len(), 4);
    assert_eq!(bounded.memories[0].text, prior.memories[0].text);
    config.classifier_max_input_bytes = u32::MAX;
    let before_required = reviewer_required_tokens(&config, &review_body(&config, &prior));
    let after_required = reviewer_required_tokens(&config, &review_body(&config, &candidate));
    assert!(after_required > before_required);
    config.classifier_max_input_tokens = after_required.ceil() as u32 - 1;
    assert!(memory_admission_fits(&config, &prior, &HashSet::new()));
    assert!(!memory_admission_fits(&config, &candidate, &HashSet::new()));
    let retired = prior.memories[0].text.clone();
    let kept = prior.memories[1].text.clone();
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":[4,2],"memories":[]}),
        )
        .unwrap();
    let raw = review_body(&config, &session).to_string();
    assert!(!raw.contains(&retired));
    assert!(raw.contains(&kept));
    let primary = serde_json::to_string(&session.render_projection(&HashSet::from([2]))).unwrap();
    assert!(!primary.contains(&retired));
    assert!(primary.contains(&kept));
}
