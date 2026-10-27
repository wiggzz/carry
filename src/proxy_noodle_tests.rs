#[test]
fn cold_extension_supersedes_covered_prefix_in_ordered_owner_at_same_second() {
    let dir = tempfile::tempdir().unwrap();
    let service = checkpoint_service(dir.path());
    let model = resolve_model(
        "gpt-6-luna",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        2048,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    let h =
        json!({"model":"gpt-6-luna","input":[{"role":"user","content":"prefix ".repeat(3000)}]});
    let mut extended = h.clone();
    extended["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"suffix"}));
    let mut branch = h.clone();
    branch["input"][0]["content"] = json!("unrelated branch ".repeat(3000));
    let at = now();
    let warm = json!({"input_tokens":4000,"input_tokens_details":{"cached_tokens":3900}});
    let cold = json!({"input_tokens":4000,"input_tokens_details":{"cached_tokens":0}});
    for body in [&branch, &h] {
        history::observe_cache(&service, body, &warm, Some(&model), "auth", at).unwrap();
    }
    let lookup = |cache: &[CacheEvidence], body: &Value| {
        crate::proxy_cache::reusable(cache, body, Some(&model), "auth", at)
    };
    let previous = history::cache(&service).unwrap();
    assert!(lookup(&previous, &extended) > 0.0);
    // Unknown receipt and another credential cannot revoke confirmed warmth.
    history::observe_cache(&service, &extended, &Value::Null, Some(&model), "auth", at).unwrap();
    history::observe_cache(&service, &extended, &cold, Some(&model), "other", at).unwrap();
    assert!(lookup(&history::cache(&service).unwrap(), &extended) > 0.0);
    history::observe_cache(&service, &extended, &cold, Some(&model), "auth", at).unwrap();
    let saved = history::cache(&service).unwrap();
    assert_eq!(
        lookup(&saved, &extended),
        0.0,
        "newer compatible cold extension revokes the covered prefix, including same-second terminals"
    );
    assert!(lookup(&saved, &branch) > 0.0);
    let restarted = checkpoint_service(dir.path());
    assert_eq!(lookup(&history::cache(&restarted).unwrap(), &extended), 0.0);
    history::observe_cache(&restarted, &h, &warm, Some(&model), "auth", at).unwrap();
    assert!(
        lookup(&history::cache(&restarted).unwrap(), &extended) > 0.0,
        "a genuinely later rewarm is not constrained forever by an old cold extension"
    );
}

#[test]
fn undispatched_review_cannot_subsidize_a_warm_primary_losing_rewrite() {
    let mut config = reviewer_config();
    config.classifier_cache_policy = ClassifierCachePolicy::Disabled;
    config.payoff_requests = 1;
    config.min_payback_percent = 0;
    let model = resolve_model(
        "gpt-6-luna",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        2048,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    config.model_resolution = Some(carry::models::ResolvedPair {
        schema_version: 1,
        primary: model.clone(),
        reviewer: model.clone(),
    });
    let mut session = semantic_subject();
    session.history[0].value["content"] = json!("protected warm direction ".repeat(6000));
    session.completed_requests = 10;
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":[2],"memories":[]}),
        )
        .unwrap();
    let mut body = json!({"model":"gpt-6-luna","input":session.render_primary(),"metadata":{"padding":"metadata ".repeat(10000)}});
    session.review_context = cache_base(&body);
    let actual = admission::primary_tokens(&config, &session, &session.review_context);
    let inflated = admission::primary_tokens(&config, &session, &body);
    assert!(inflated > actual);
    config.review_min_primary_tokens = ((actual + inflated) / 2.0) as u32;
    assert_eq!(
        admission::decision(&config, &session, &session.review_context, false)["reason"],
        "primary_floor"
    );
    let wire = primary_wire_body(&config, &body);
    let projection = planning::Projection {
        omitted: HashSet::from([2]),
        input: session.render_projection(&HashSet::from([2])),
    };
    body["input"] = json!(projection.input);
    let compact = primary_wire_body(&config, &body);
    body["input"] = json!(session.render_primary());
    let mut found = None;
    // Choose a true tariff/cache boundary using the actual shared cost function,
    // not a hand-built planner. Then compare the actual selected action.
    for cached in (0..=10000).step_by(25) {
        session.upstream_cache.clear();
        crate::proxy_cache::observe(
            &mut session.upstream_cache,
            &wire,
            &json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":cached}}),
            Some(&model),
            &config.primary_cache_auth,
            now(),
        );
        let p = resolved_view_costs(
            &wire,
            &compact,
            &session.upstream_cache,
            "gpt-6-luna",
            1,
            Some(&model),
            &config.primary_cache_auth,
        )
        .unwrap();
        let review_keep = review_body(&config, &session);
        let review_compact = review_body_projected(&config, &session, &projection);
        let rates = reviewer_rates(&config, estimate(&review_keep)).unwrap();
        let savings = (estimate(&review_keep) - estimate(&review_compact)) * rates.input;
        if p.compact > p.keep && p.compact - p.keep < savings * 0.9 {
            found = Some((p, cached));
            break;
        }
    }
    let (primary, cached) = found
        .expect("real rendered warm-primary loss smaller than otherwise credited reviewer saving");
    assert!(primary.compact > primary.keep);
    let (selected, report) = plan(&config, &session, &body);
    assert!(
        selected.is_none(),
        "floor suppresses review dispatch; warm-primary losing rewrite must not be selected: cached={cached}, report={report}"
    );
    let joint = &report["candidates"][0]["selection_joint"];
    assert_eq!(joint["future_shadow_keep"], 0.0);
    assert_eq!(joint["future_shadow_compact"], 0.0);
    config.review_min_primary_tokens = 0;
    let (selected, report) = plan(&config, &session, &body);
    assert!(
        selected.is_some(),
        "genuinely due review saving remains priced: {report}"
    );
    assert!(
        report["candidates"][0]["selection_joint"]["future_shadow_keep"]
            .as_f64()
            .unwrap()
            > 0.0
    );
}

#[test]
fn settled_old_review_cannot_resurrect_bootstrap_across_reset_or_namespace() {
    let mut before = semantic_subject();
    before.bootstrap_review_pending = true;
    before.last_review_request = 7;
    for boundary in ["completed", "direction", "namespace"] {
        let mut current = before.clone();
        current.bootstrap_review_pending = false;
        match boundary {
            "direction" => current.reset_active(),
            "namespace" => current.history_namespace = Some("other-namespace".into()),
            _ => {}
        }
        current.last_review_request = 20;
        settle_review(&mut current, &before, &before, &ReviewRun::default());
        assert!(
            !current.bootstrap_review_pending,
            "stale {boundary} settlement cannot recreate retry authority"
        );
        if boundary != "completed" {
            assert_eq!(current.last_review_request, 20);
        }
    }
}
