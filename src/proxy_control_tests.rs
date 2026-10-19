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
