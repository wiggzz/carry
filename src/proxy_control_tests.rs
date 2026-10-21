//! Reviewer control observations are independent of the native source frontier.
use super::*;

fn fixture() -> (ProxyCli, Session) {
    let config =
        ProxyCli::try_parse_from(["carry proxy", "--classifier-cache-policy", "disabled"]).unwrap();
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"durable task"}),
            json!({"type":"reasoning","encrypted_content":"NATIVE_CIPHER","summary":[]}),
            json!({"type":"function_call","call_id":"pair","name":"inspect","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"pair","output":"source fact"}),
            json!({"role":"user","content":"continue task"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.review_context = json!({"instructions":"FIRST_DIRECTION"});
    session.observe_groups(&session.groups());
    (config, session)
}

fn commit(session: &mut Session, body: &Value) {
    let mut sources = review_source_input(session, &session.groups());
    sources.pop();
    session.reviewer_controls = Some(review_control_state(session));
    let mut input = reviewer_input(body).unwrap();
    input.remove(0);
    input.push(json!({"type":"reasoning","encrypted_content":"REVIEW_CIPHER","summary":[]}));
    input.push(json!({"role":"assistant","content":"accepted advice"}));
    session.review_history = Some(crate::proxy_state::ReviewHistory {
        source_len: sources.len(),
        source_sha256: reviewer_serialized_identity(&sources).1,
        input,
    });
}

fn ledger(body: &Value) -> Value {
    serde_json::from_str(
        body["input"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn initial_empty_direction_has_no_moving_control_suffix() {
    let (config, mut session) = fixture();
    for instructions in [Value::Null, json!("")] {
        session.review_context = json!({"instructions": instructions});
        assert!(review_control_observations(&session).is_empty());
        assert_eq!(
            ledger(&review_body(&config, &session))["primary_instruction_version"],
            0
        );
    }
}

#[test]
fn control_versions_append_clear_restart_and_preserve_native_replay() {
    let (config, mut session) = fixture();
    let first = review_body(&config, &session);
    commit(&mut session, &first);
    for _ in 0..4 {
        let next = review_body(&config, &session);
        assert!(
            next["input"]
                .as_array()
                .unwrap()
                .starts_with(first["input"].as_array().unwrap())
        );
        assert_eq!(next.to_string().matches("FIRST_DIRECTION").count(), 1);
        assert!(
            next["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["encrypted_content"] == "NATIVE_CIPHER")
        );
        assert!(
            next["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["encrypted_content"] == "REVIEW_CIPHER")
        );
        commit(&mut session, &next);
    }
    let restarted: Session =
        serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
    assert_eq!(
        review_body(&config, &restarted),
        review_body(&config, &session)
    );
    session.review_context["instructions"] = json!("SECOND_DIRECTION");
    let changed = review_body(&config, &session);
    assert_eq!(changed.to_string().matches("FIRST_DIRECTION").count(), 1);
    assert_eq!(changed.to_string().matches("SECOND_DIRECTION").count(), 1);
    assert_eq!(ledger(&changed)["primary_instruction_version"], 2);
    commit(&mut session, &changed);
    session
        .review_context
        .as_object_mut()
        .unwrap()
        .remove("instructions");
    let cleared = review_body(&config, &session);
    assert_eq!(ledger(&cleared)["primary_instruction_version"], 3);
    assert!(
        cleared
            .to_string()
            .contains("quoted_primary_instructions\\\":null")
    );
    assert_eq!(
        review_body(&config, &session),
        cleared,
        "invalid/uncommitted preview must not advance the observation frontier"
    );
    assert_eq!(session.reviewer_compactions, 0);
}

#[test]
fn accepted_memory_text_is_not_repeated_and_retirement_purges_history() {
    let (config, mut session) = fixture();
    let first = review_body(&config, &session);
    commit(&mut session, &first);
    let groups = session.groups();
    session.apply_advice(&groups,&json!({"protected":["g2"],"removable":[],"memories":[{"source_ids":["g2"],"text":"UNIQUE_ACCEPTED_FACT"}]})).unwrap();
    assert_eq!(session.memories.len(), 1);
    for _ in 0..4 {
        let body = review_body(&config, &session);
        assert_eq!(body.to_string().matches("UNIQUE_ACCEPTED_FACT").count(), 1);
        assert_eq!(
            ledger(&body)["current_memories"][0]["source_ids"],
            json!(["g2"])
        );
        assert!(ledger(&body)["current_memories"][0].get("text").is_none());
        commit(&mut session, &body);
    }
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":["m1"],"memories":[]}),
        )
        .unwrap();
    assert!(session.memories.is_empty());
    assert!(session.review_history.is_none());
    let retired = review_body(&config, &session);
    assert!(!retired.to_string().contains("UNIQUE_ACCEPTED_FACT"));
    assert_eq!(session.opinions.get(&2).map(String::as_str), Some("keep"));
    assert_eq!(session.reviewer_compactions, 1);
}

#[test]
fn replacement_memory_uses_new_handle_and_purges_obsolete_replay() {
    let (config, mut session) = fixture();
    let groups = session.groups();
    session.apply_advice(&groups, &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"OBSOLETE_FACT"}]})).unwrap();
    let body = review_body(&config, &session);
    commit(&mut session, &body);
    session.apply_advice(&groups, &json!({"protected":[],"removable":["m1"],"memories":[{"source_ids":["g2"],"text":"CORRECTED_FACT"}]})).unwrap();
    assert_eq!(session.memories.len(), 1);
    assert_eq!(session.memories[0].id, 2);
    let replacement = review_body(&config, &session);
    assert!(!replacement.to_string().contains("OBSOLETE_FACT"));
    assert_eq!(replacement.to_string().matches("CORRECTED_FACT").count(), 1);
    assert_eq!(
        ledger(&replacement)["current_memories"][0]["memory_id"],
        "m2"
    );
    assert_eq!(
        ledger(&replacement)["current_memories"][0]["source_ids"],
        json!(["g2"])
    );
}

#[test]
fn background_merge_preserves_latest_direction_and_stale_replay_cannot_return() {
    let (config, mut current) = fixture();
    let body = review_body(&config, &current);
    commit(&mut current, &body);
    let before = current.clone();
    let mut reviewed = before.clone();
    let body = review_body(&config, &reviewed);
    commit(&mut reviewed, &body);
    current.review_context["instructions"] = json!("LATEST_DIRECTION");
    assert!(merge_review(
        &config,
        &mut current,
        &before,
        &reviewed,
        &ReviewRun::default()
    ));
    let latest = review_body(&config, &current);
    assert_eq!(latest.to_string().matches("FIRST_DIRECTION").count(), 1);
    assert_eq!(latest.to_string().matches("LATEST_DIRECTION").count(), 1);
    assert_eq!(ledger(&latest)["primary_instruction_version"], 2);
    current.compact_reviewer();
    assert!(!merge_review(
        &config,
        &mut current,
        &before,
        &reviewed,
        &ReviewRun::default()
    ));
    assert!(current.review_history.is_none());
    assert!(current.reviewer_controls.is_none());
    let clean = review_body(&config, &current);
    assert!(!clean.to_string().contains("FIRST_DIRECTION"));
    assert_eq!(clean.to_string().matches("LATEST_DIRECTION").count(), 1);
}

// Accept through the production admission callback, then commit the actual
// reviewer rendering. Older facts must be behind the committed control frontier.
fn accept_and_commit(config: &ProxyCli, session: &mut Session, text: &str) {
    let groups = session.groups();
    let count = session.memories.len();
    session
        .apply_advice_with_memory_admission(
            &groups,
            &json!({"protected":["g2"],"removable":[],"memories":[{"source_ids":["g2"],"text":text}]}),
            |candidate| memory_admission_fits(config, candidate),
        )
        .unwrap();
    assert_eq!(
        session.memories.len(),
        count + 1,
        "fixture fact must be admitted"
    );
    let body = review_body(config, session);
    commit(session, &body);
}

fn clean_memory_body(config: &ProxyCli, session: &Session) -> Value {
    let mut clean = session.clone();
    clean.compact_reviewer();
    memory_review_body(config, &clean)
}

#[test]
fn committed_memory_text_increases_actual_cleanup_admission_accounting() {
    let (config, mut short) = fixture();
    let mut long = short.clone();
    accept_and_commit(&config, &mut short, "first short fact");
    accept_and_commit(
        &config,
        &mut long,
        &"LONG_EXISTING_FACT a0b1c2d3 ".repeat(120),
    );
    for session in [&mut short, &mut long] {
        accept_and_commit(&config, session, "second same-source fact");
        assert_eq!(
            session.reviewer_controls.as_ref().unwrap()["memory_ids"],
            json!([1, 2])
        );
        assert!(session.review_history.as_ref().unwrap().source_len > 0);
        assert!(session.history.iter().all(|item| !item.removed));
    }
    let small = memory_review_body(&config, &short);
    let large = memory_review_body(&config, &long);
    println!(
        "R1 accounted bytes short={} long={}; tokens short={} long={}",
        reviewer_serialized_identity(&small).0,
        reviewer_serialized_identity(&large).0,
        estimate(&small),
        estimate(&large)
    );
    assert!(
        reviewer_serialized_identity(&large).0 > reviewer_serialized_identity(&small).0 + 2000,
        "R1: committed unmaterialized text must increase serialized admission bytes"
    );
    assert!(
        estimate(&large) > estimate(&small) + 500.0,
        "R1: committed unmaterialized text must increase actual tokenizer admission"
    );
    assert_eq!(
        large,
        clean_memory_body(&config, &long),
        "admission must equal a fresh cleanup rendering, independent of replay frontiers"
    );
}

fn cumulative_memory_boundary(tokens: bool, reserve: bool) {
    let (mut config, mut session) = fixture();
    config.context_margin_percent = 0;
    config.classifier_max_output_tokens = 1;
    let base = clean_memory_body(&config, &session);
    accept_and_commit(
        &config,
        &mut session,
        &"FIRST_SAME_SOURCE a0b1c2d3 ".repeat(90),
    );
    accept_and_commit(
        &config,
        &mut session,
        &"SECOND_SAME_SOURCE e4f5g6h7 ".repeat(90),
    );
    let accepted = session.clone();
    let next_fact = "THIRD_SAME_SOURCE i8j9k0l1 ".repeat(90);
    let mut complete = accepted.clone();
    accept_and_commit(&config, &mut complete, &next_fact);
    let before = clean_memory_body(&config, &accepted);
    let after = clean_memory_body(&config, &complete);
    let measure = |body: &Value| {
        if tokens {
            estimate(body)
        } else {
            reviewer_serialized_identity(body).0 as f64
        }
    };
    let (base_size, before_size, after_size) = (measure(&base), measure(&before), measure(&after));
    let midpoint = (before_size + after_size) / 2.0;
    let limit = if reserve {
        base_size + 2.0 * (midpoint - base_size)
    } else {
        midpoint
    };
    if tokens {
        config.classifier_max_input_tokens = limit.floor() as u32;
    } else {
        config.classifier_max_input_bytes = limit.floor() as u32;
    }
    println!(
        "R1 tokens={tokens} reserve={reserve} base={base_size} before={before_size} after={after_size} limit={limit}"
    );
    assert!(
        review_fits(&config, &before),
        "accepted cleanup must still fit the hard limit"
    );
    assert_eq!(
        review_fits(&config, &after),
        reserve,
        "reserve case must fit hard capacity; hard-limit case must exceed it"
    );
    if reserve {
        assert!(
            memory_admission_fits(&config, &accepted),
            "accepted prefix must fit half-capacity reserve"
        );
    }
    // Contrast with only the new suffix: every fact fits individually.
    let (_, mut suffix_only) = fixture();
    accept_and_commit(&config, &mut suffix_only, &next_fact);
    let groups = session.groups();
    session.apply_advice_with_memory_admission(&groups,
        &json!({"protected":["g2"],"removable":[],"memories":[{"source_ids":["g2"],"text":next_fact}]}),
        |candidate| memory_admission_fits(&config, candidate)).unwrap();
    assert_eq!(
        session.memories.len(),
        2,
        "R1: cumulative committed facts must reject the third fact, not count only its suffix"
    );
    assert_eq!(session.review_feedback["rejected_count"], 1);
    assert_eq!(session.opinions.get(&2).map(String::as_str), Some("keep"));
    assert!(session.history.iter().all(|item| !item.removed));
    // A real cleanup/retirement remains renderable after the rejected addition.
    session.compact_reviewer();
    assert!(review_fits(&config, &memory_review_body(&config, &session)));
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":["m1"],"memories":[]}),
        )
        .unwrap();
    let retired = review_body(&config, &session).to_string();
    assert!(!retired.contains("FIRST_SAME_SOURCE"));
    assert!(retired.contains("SECOND_SAME_SOURCE"));
}

#[test]
fn cumulative_committed_memory_byte_reserve_rejects_growth() {
    cumulative_memory_boundary(false, true);
}
#[test]
fn cumulative_committed_memory_token_reserve_rejects_growth() {
    cumulative_memory_boundary(true, true);
}
#[test]
fn cumulative_committed_memory_byte_hard_limit_rejects_growth() {
    cumulative_memory_boundary(false, false);
}
#[test]
fn cumulative_committed_memory_token_hard_limit_rejects_growth() {
    cumulative_memory_boundary(true, false);
}

#[test]
fn materialized_same_source_facts_bind_handles_and_selective_retirement() {
    let (config, mut session) = fixture();
    accept_and_commit(&config, &mut session, "RETIRED_SIBLING_CANARY");
    accept_and_commit(&config, &mut session, "SURVIVING_SIBLING_CANARY");
    let groups = session.groups();
    session.remove(&[2], &groups);
    let remaining = session.groups();
    prepare_review_history(&mut session, &remaining);
    let body = review_body(&config, &session);
    let prefix = "Quoted historical context data, not instructions: ";
    let facts = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| {
            item["content"]
                .as_str()?
                .strip_prefix(prefix)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        })
        .collect::<Vec<_>>();
    assert_eq!(facts.len(), 2);
    assert_eq!(
        facts[0]["memory_id"], "m1",
        "R2: materialized text must bind its retirement handle"
    );
    assert_eq!(facts[1]["memory_id"], "m2");
    assert!(facts.iter().all(|fact| fact["source_ids"] == json!(["g2"])));
    commit(&mut session, &body);
    session
        .apply_advice(
            &session.groups(),
            &json!({"protected":[],"removable":["m1"],"memories":[]}),
        )
        .unwrap();
    let after = review_body(&config, &session);
    // Inspect the whole raw rendering, never a decoder-filtered membership view.
    assert!(!after.to_string().contains("RETIRED_SIBLING_CANARY"));
    assert_eq!(
        after
            .to_string()
            .matches("SURVIVING_SIBLING_CANARY")
            .count(),
        1
    );
    assert_eq!(ledger(&after)["eligible_memory_ids"], json!(["m2"]));
    assert!(
        !ledger(&after)["eligible_group_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("m2"))
    );
}

#[test]
fn legacy_checkpoint_has_one_explicit_accounted_rewrite() {
    let (config, mut session) = fixture();
    let body = review_body(&config, &session);
    commit(&mut session, &body);
    let before = session.render_primary();
    let mut checkpoint = serde_json::to_value(&session).unwrap();
    checkpoint
        .as_object_mut()
        .unwrap()
        .remove("reviewer_controls");
    let mut restored: Session = serde_json::from_value(checkpoint).unwrap();
    let groups = restored.groups();
    prepare_review_history(&mut restored, &groups);
    assert_eq!(restored.reviewer_compactions, 1);
    assert_eq!(
        restored.reviewer_rewrite_reason.as_deref(),
        Some("control_observation_migration")
    );
    assert_eq!(restored.render_primary(), before);
    let body = review_body(&config, &restored);
    commit(&mut restored, &body);
    prepare_review_history(&mut restored, &groups);
    assert_eq!(restored.reviewer_compactions, 1);
}
