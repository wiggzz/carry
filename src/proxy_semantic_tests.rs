// Direct controls for the prefix-advice/request-projection model.
include!("proxy_review_boundary_tests.rs");
#[tokio::test]
async fn reviewer_deadline_includes_subscription_auth_wait_and_censors_spend() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.codex_login = true;
    service.config.classifier_url = None;
    service.config.codex_home = Some(directory.path().to_path_buf());
    service.config.classifier_timeout_secs = 1;
    let mut session = semantic_subject();
    let projection = planning::Projection::canonical(&session);
    let mut run = ReviewRun::default();
    // The auth lock is also taken during a 401 refresh. No network or real
    // credential is involved: this exercises the production subscription wait.
    let _auth = session_lock(&service, "codex-auth").await;
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        run_review(&service, "deadline", &mut session, &projection, &mut run),
    )
    .await;
    assert!(
        result.is_ok(),
        "review deadline must include subscription auth waits"
    );
    assert!(!result.unwrap());
    assert_eq!(session.shadow.calls, 1);
    assert_eq!(
        session.shadow.unavailable_cost_calls, 1,
        "cancelled work is not free"
    );
    assert!(!run.accepted);
}

fn semantic_subject() -> Session {
    let mut s = Session::default();
    s.ingest(&[
        json!({"role":"user","content":"goal"}),
        json!({"role":"assistant","content":"large evidence ".repeat(3000)}),
        json!({"role":"user","content":"current task"}),
    ])
    .unwrap();
    for item in &mut s.history {
        item.exposed = true;
    }
    s.review_context = json!({"model":"gpt-6-luna"});
    s.history_namespace = Some("fixture-owner".into());
    s
}

#[test]
fn planner_prices_only_scheduled_review_views_including_output_cap() {
    let mut config = reviewer_config();
    config.classifier_cache_policy = ClassifierCachePolicy::Disabled;
    let mut session = semantic_subject();
    session.bootstrap_review_pending = true;
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":["g2"],"memories":[]}),
        )
        .unwrap();
    let projection = planning::Projection {
        omitted: HashSet::from([2]),
        input: session.render_projection(&HashSet::from([2])),
    };
    let keep = review_body(&config, &session);
    let compact = review_body_projected(&config, &session, &projection);
    let body = json!({"model":"gpt-6-luna","input":session.render_primary()});
    for compact_only in [true, false] {
        config.classifier_max_input_tokens = if compact_only {
            reviewer_required_tokens(&config, &compact).ceil() as u32
        } else {
            reviewer_required_tokens(&config, &keep).ceil() as u32
        };
        assert!(review_fits(&config, &compact));
        assert_eq!(review_fits(&config, &keep), !compact_only);
        let (_, report) = plan(&config, &session, &body);
        let joint = &report["candidates"][0]["selection_joint"];
        for (view, field, scheduled) in [
            (&keep, "future_shadow_keep", !compact_only),
            (&compact, "future_shadow_compact", true),
        ] {
            let rates = reviewer_rates(&config, estimate(view)).unwrap();
            let expected = if scheduled {
                estimate(view) * rates.input
                    + config.classifier_max_output_tokens as f64 * rates.output
            } else {
                0.0
            };
            let actual = joint[field].as_f64().unwrap();
            assert!(
                (actual - expected).abs() < 1e-12,
                "{field}: scheduled={scheduled}, actual={actual}, expected={expected}"
            );
        }
    }
}

#[test]
fn canonical_source_survives_disposable_projection_and_owner_serialization() {
    let config = reviewer_config();
    let mut s = semantic_subject();
    let canonical = s.render_primary();
    s.apply_advice(&s.groups(),&json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"fact"}]})).unwrap();
    let record = semantic::Record::capture(&s);
    let omitted = HashSet::from([2]);
    let projection = s.render_projection(&omitted);
    assert!(!projection.contains(&canonical[1]));
    assert!(json!(projection).to_string().contains("fact"));
    assert_eq!(s.render_primary(), canonical);
    let serialized = serde_json::to_value(&s).unwrap();
    assert!(serialized.get("opinions").is_none() && serialized.get("memories").is_none());
    let mut restored: Session = serde_json::from_value(serialized).unwrap();
    record.apply(&mut restored);
    assert!(restored.drop_authorized(&restored.groups()[1]));
    assert_eq!(restored.render_primary(), canonical);
    assert_eq!(config.history_max_age_secs, 604800);
}

#[test]
fn merged_projected_group_authorizes_original_member_union_not_separator() {
    let mut s = Session::default();
    s.ingest(&[
        json!({"role":"user","content":"goal"}),
        json!({"role":"assistant","content":"first"}),
        json!({"role":"user","content":"separator"}),
        json!({"role":"assistant","content":"second"}),
        json!({"role":"user","content":"task"}),
    ])
    .unwrap();
    for item in &mut s.history {
        item.exposed = true;
    }
    let groups = s.groups_for(&HashSet::from([3]));
    assert_eq!(
        groups.iter().find(|g| g.id == 2).unwrap().members,
        vec![2, 4]
    );
    s.apply_advice(&groups,&json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"union fact"}]})).unwrap();
    assert_eq!(s.memories[0].source_ids, vec![2, 4]);
    assert_eq!(s.memory_anchors[&s.memories[0].id], 4);
    let r = semantic::Record::capture(&s);
    let mut restored = s.clone();
    r.apply(&mut restored);
    assert!(
        restored
            .groups()
            .iter()
            .filter(|g| [2, 4].contains(&g.id))
            .all(|g| restored.drop_authorized(g))
    );
    assert!(!restored.drop_authorized(&restored.groups()[2]));
    let sent = restored.render_projection(&HashSet::from([2, 4]));
    assert_eq!(sent.last().unwrap()["content"], "task");
    assert!(
        sent.iter()
            .position(|i| i.to_string().contains("union fact"))
            .unwrap()
            > sent
                .iter()
                .position(|i| i["content"] == "separator")
                .unwrap()
    );
}

fn memory_provenance_on_wire(body: &Value, id: &str) -> (Value, Value) {
    let records: Vec<Value> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| {
            item["content"]
                .as_str()
                .or_else(|| item["content"][0]["text"].as_str())
                .and_then(|text| serde_json::from_str(text).ok())
        })
        .collect();
    let fact = records
        .iter()
        .find(|r| r["memory_id"] == id && r["text"].is_string())
        .unwrap();
    let ledger = records
        .iter()
        .find(|r| r["current_memories"].is_array())
        .unwrap();
    let entry = ledger["current_memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["memory_id"] == id)
        .unwrap();
    (fact["source_ids"].clone(), entry["source_ids"].clone())
}

fn provenance_tool_subject() -> Session {
    let mut s = Session::default();
    s.ingest(&[
        json!({"role":"user","content":"goal"}),
        json!({"type":"reasoning","encrypted_content":"opaque","summary":[]}),
        json!({"type":"function_call","call_id":"f","name":"inspect","arguments":"{}"}),
        json!({"type":"custom_tool_call","call_id":"c","name":"inspect","input":"raw"}),
        json!({"type":"function_call_output","call_id":"f","output":"function evidence"}),
        json!({"type":"custom_tool_call_output","call_id":"c","output":"custom evidence"}),
        json!({"role":"user","content":"current"}),
    ])
    .unwrap();
    for i in &mut s.history {
        i.exposed = true;
    }
    s.apply_advice(&s.groups(), &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"complete native fact"}]})).unwrap();
    assert_eq!(s.memories[0].source_ids, vec![2, 3, 4, 5, 6]);
    s
}

#[test]
fn reviewer_memory_provenance_names_complete_native_group_and_preserves_strict_eligibility() {
    let config = reviewer_config();
    let mut s = provenance_tool_subject();
    let original = s.render_primary();
    let (sources, ledger) = memory_provenance_on_wire(&review_body(&config, &s), "m1");
    assert_eq!(
        sources,
        json!(["g2"]),
        "members are not independently actionable groups"
    );
    assert_eq!(ledger, sources);
    s.apply_advice(&s.groups(), &json!({"protected":[],"removable":[],"memories":[{"source_ids":sources,"text":"derived native fact"}]})).unwrap();
    assert_eq!(s.review_feedback["rejected_count"], 0);
    assert_eq!(s.memories[1].source_ids, vec![2, 3, 4, 5, 6]);
    let omitted = HashSet::from([2, 3, 4, 5, 6]);
    let projection = planning::Projection {
        input: s.render_projection(&omitted),
        omitted,
    };
    let body = review_body_projected(&config, &s, &projection);
    assert_eq!(
        memory_provenance_on_wire(&body, "m1"),
        (json!(["g2"]), json!(["g2"]))
    );
    for ids in [json!(["g2"]), json!([2]), json!(["m1"]), json!(["g999"])] {
        s.apply_advice(&s.groups_for(&projection.omitted), &json!({"protected":[],"removable":[],"memories":[{"source_ids":ids,"text":"not newly eligible"}]})).unwrap();
        assert_eq!(s.review_feedback["rejected_count"], 1);
        assert_eq!(s.memories.len(), 2);
    }
    assert_eq!(s.render_primary(), original);
}

#[test]
fn reviewer_memory_provenance_resolves_merged_split_and_omitted_original_groups() {
    let config = reviewer_config();
    let mut s = Session::default();
    s.ingest(&[
        json!({"role":"user","content":"goal"}),
        json!({"role":"assistant","content":"first evidence"}),
        json!({"role":"user","content":"separator"}),
        json!({"role":"assistant","content":"second evidence"}),
        json!({"role":"user","content":"current"}),
    ])
    .unwrap();
    for i in &mut s.history {
        i.exposed = true;
    }
    let original = s.render_primary();
    s.apply_advice(&s.groups_for(&HashSet::from([3])), &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"joined fact"}]})).unwrap();
    assert_eq!(s.memories[0].source_ids, vec![2, 4]);
    let record = semantic::Record::capture(&s);
    let mut restored: Session = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    record.apply(&mut restored);
    for (omitted, expected) in [
        (HashSet::new(), json!(["g2", "g4"])),
        (HashSet::from([3]), json!(["g2"])),
        (HashSet::from([2]), json!(["g2", "g4"])),
        (HashSet::from([2, 4]), json!(["g2", "g4"])),
    ] {
        let projection = planning::Projection {
            input: restored.render_projection(&omitted),
            omitted,
        };
        let body = review_body_projected(&config, &restored, &projection);
        assert_eq!(
            memory_provenance_on_wire(&body, "m1"),
            (expected.clone(), expected.clone())
        );
        let mut candidate = restored.clone();
        candidate.apply_advice(&candidate.groups_for(&projection.omitted), &json!({"protected":[],"removable":[],"memories":[{"source_ids":expected,"text":"derived joined fact"}]})).unwrap();
        if projection.omitted.contains(&2) {
            assert_eq!(candidate.review_feedback["rejected_count"], 1);
            assert_eq!(candidate.memories.len(), 1);
        } else {
            assert_eq!(candidate.review_feedback["rejected_count"], 0);
            assert_eq!(candidate.memories[1].source_ids, vec![2, 4]);
        }
    }
    assert_eq!(restored.render_primary(), original);
    assert_eq!(restored.memory_anchors[&restored.memories[0].id], 4);
}

#[test]
fn repeated_native_fact_is_deduplicated_in_original_member_coordinates() {
    let mut s = provenance_tool_subject();
    let next = s.next_memory_id;
    s.apply_advice(&s.groups(), &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"complete native fact"}]})).unwrap();
    assert_eq!(s.review_feedback["rejected_count"], 0);
    assert_eq!(
        s.memories.len(),
        1,
        "group identity must normalize before comparison to stored member provenance"
    );
    assert_eq!(s.next_memory_id, next);
}

#[test]
fn native_suffix_dependency_extension_rejects_drop_and_fact_but_unrelated_growth_does_not() {
    for ty in ["function_call", "custom_tool_call"] {
        let mut s = Session::default();
        let mut h = vec![
            json!({"role":"user","content":"goal"}),
            json!({"type":ty,"call_id":"x","name":"tool","arguments":"{}"}),
            json!({"type":format!("{ty}_output"),"call_id":"x","output":"done"}),
            json!({"role":"user","content":"task"}),
        ];
        s.ingest(&h).unwrap();
        for i in &mut s.history {
            i.exposed = true;
        }
        s.apply_advice(&s.groups(),&json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"fact"}]})).unwrap();
        let r = semantic::Record::capture(&s);
        h.push(json!({"role":"user","content":"unrelated"}));
        s.ingest(&h).unwrap();
        r.apply(&mut s);
        assert!(s.groups().iter().any(|g| s.drop_authorized(g)));
        assert_eq!(s.memories.len(), 1);
        h.push(json!({"type":format!("{ty}_output"),"call_id":"x","output":"late duplicate"}));
        s.ingest(&h).unwrap();
        r.apply(&mut s);
        assert!(!s.groups().iter().any(|g| s.drop_authorized(g)));
        assert!(s.memories.is_empty());
    }
}

#[test]
fn generic_wire_cache_has_no_purpose_and_requires_exact_actual_namespace() {
    let primary = resolve_model(
        "gpt-6-luna",
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        2048,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    let reviewer = primary.clone();
    let body = json!({"model":"gpt-6-luna","instructions":"direction","prompt_cache_key":"affinity","input":[{"role":"user","content":"data ".repeat(4000)}]});
    let usage = json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":9000}});
    let mut cache = Vec::new();
    crate::proxy_cache::observe(&mut cache, &body, &usage, Some(&reviewer), "same-auth", 100);
    let lookup = |body: &Value, r: &ResolvedModel, key: &str, at| {
        crate::proxy_cache::reusable(&cache, body, Some(r), key, at)
    };
    assert!(
        lookup(&body, &primary, "same-auth", 101) > 0.0,
        "reviewer receipt is primary evidence for identical wire"
    );
    for (field, value) in [
        ("model", json!("gpt-6-sol")),
        ("instructions", json!("different")),
        ("prompt_cache_key", json!("different")),
        ("reasoning", json!({"effort":"high"})),
        ("max_output_tokens", json!(99)),
    ] {
        let mut changed = body.clone();
        changed[field] = value;
        assert_eq!(lookup(&changed, &primary, "same-auth", 101), 0.0);
    }
    let mut changed = body.clone();
    changed["input"][0]["content"] = json!("different");
    assert_eq!(lookup(&changed, &primary, "same-auth", 101), 0.0);
    assert_eq!(lookup(&body, &primary, "different-auth", 101), 0.0);
    assert_eq!(lookup(&body, &primary, "same-auth", 1900), 0.0);
    assert_eq!(lookup(&body, &primary, "same-auth", 99), 0.0);
    let mut route = primary.clone();
    route.route = Route::new("https://other.invalid/v1/responses", false);
    assert_eq!(lookup(&body, &route, "same-auth", 101), 0.0);
    route = primary.clone();
    route.cache = None;
    assert_eq!(lookup(&body, &route, "same-auth", 101), 0.0);
    let mut unknown = Vec::new();
    crate::proxy_cache::observe(
        &mut unknown,
        &body,
        &Value::Null,
        Some(&primary),
        "same-auth",
        100,
    );
    assert!(unknown.is_empty());
    crate::proxy_cache::observe(
        &mut unknown,
        &body,
        &json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":0}}),
        Some(&primary),
        "same-auth",
        100,
    );
    assert_eq!(
        crate::proxy_cache::reusable(&unknown, &body, Some(&primary), "same-auth", 101),
        0.0
    );
}

#[tokio::test]
async fn exact_prefix_empty_completion_supersedes_old_fact_and_stale_generation() {
    let dir = tempfile::tempdir().unwrap();
    let service = checkpoint_service(dir.path());
    let mut s = semantic_subject();
    s.review_epoch = 10;
    s.apply_advice(&s.groups(),&json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"retire me"}]})).unwrap();
    history::publish(&service, &s, &s).unwrap();
    let mut latest = s.clone();
    latest.review_epoch = 11;
    latest.apply_advice(&latest.groups(),&json!({"protected":[],"removable":[format!("m{}",latest.memories[0].id)],"memories":[]})).unwrap();
    history::publish(&service, &s, &latest).unwrap();
    history::publish(&service, &s, &s).unwrap();
    let h = s.render_primary();
    let r = history::advice(&service, "fixture-owner", &h)
        .unwrap()
        .unwrap();
    assert_eq!(r.generation, 11);
    assert!(r.memories.is_empty());
    let mut sibling = h.clone();
    sibling.push(json!({"role":"user","content":"B"}));
    assert_eq!(
        history::advice(&service, "fixture-owner", &sibling)
            .unwrap()
            .unwrap()
            .generation,
        11
    );
    let mut divergent = h.clone();
    divergent[2]["content"] = json!("A");
    assert!(
        history::advice(&service, "fixture-owner", &divergent)
            .unwrap()
            .is_none()
    );
}

#[test]
fn explicit_review_cache_boundary_excludes_current_ledger_and_preserves_native_source() {
    let config = reviewer_config();
    let mut session = semantic_subject();
    let body = review_body(&config, &session);
    let input = body["input"].as_array().unwrap();
    for source in session.render_primary() {
        assert!(
            input.contains(&source),
            "cache controls cannot alter native source values"
        );
    }
    let boundaries: Vec<usize> = input
        .iter()
        .enumerate()
        .filter_map(|(n, item)| {
            item["content"]
                .as_array()
                .is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|block| block.get("prompt_cache_breakpoint").is_some())
                })
                .then_some(n + 1)
        })
        .collect();
    assert_eq!(
        boundaries,
        vec![input.len() - 1],
        "write stable observations, never the current ledger"
    );
    session.review_feedback = json!({"accepted":1});
    let changed = review_body(&config, &session);
    assert_ne!(body["input"], changed["input"]);
    let model = resolve_model(
        &config.classifier_model,
        Route::new("https://api.openai.com/v1/responses", false),
        None,
        None,
        2048,
        0,
    )
    .unwrap()
    .with_scenario_cache_policy();
    let mut cache = Vec::new();
    crate::proxy_cache::observe(
        &mut cache,
        &body,
        &json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":9000}}),
        Some(&model),
        "same-auth",
        100,
    );
    assert!(
        crate::proxy_cache::reusable(&cache, &changed, Some(&model), "same-auth", 101) > 0.0,
        "actual stable-prefix receipt remains usable after ledger-only changes"
    );
}
#[test]
fn removed_synchronous_compatibility_flag_is_rejected() {
    assert!(
        ProxyCli::try_parse_from(["carry proxy", "--background-reviews=false"]).is_err(),
        "ordinary review is always asynchronous; do not retain a no-op synchronous compatibility mode"
    );
}
