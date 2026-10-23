// Synthetic-only source selection, replacement facts and checkpoint round trips.
fn omission_notice() -> Value {
    json!({"role":"user","content":"[Earlier conversation omitted by Carry; retained messages remain in original order.]"})
}

fn omission_fixture() -> (Session, Vec<Value>) {
    let input = vec![
        json!({"role":"user","content":"Keep the acceptance criteria."}),
        json!({"role":"assistant","content":"Completed: parser fix verified by fixture A; artifact reports/parser.json."}),
        json!({"role":"user","content":"Continue the separate migration."}),
        json!({"type":"reasoning","encrypted_content":"synthetic-opaque","summary":[]}),
        json!({"type":"function_call","call_id":"inspect","name":"inspect","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"inspect","output":"Migration unfinished on feat/migration; patch artifacts/migration.diff; next run fixture B; blocked on schema decision."}),
        json!({"role":"user","content":"Keep working without repeating completed checks."}),
    ];
    let mut state = Session::default();
    state.ingest(&input).unwrap();
    for item in &mut state.history {
        item.exposed = true;
    }
    state.observe_groups(&state.groups());
    (state, input)
}

#[test]
fn omission_outcome_and_unfinished_memories_survive_selection_and_restart() {
    let (mut state, input) = omission_fixture();
    let outcome = "Parser fix complete; fixture A passed; reports/parser.json.";
    let unfinished = "Migration unfinished on feat/migration; artifacts/migration.diff; next run fixture B; schema decision unresolved.";
    let groups = state.groups();
    state
        .apply_advice(
            &groups,
            &json!({"protected":["g1","g3"],"removable":["g2","g4"],"memories":[
                {"source_ids":["g2"],"text":outcome},
                {"source_ids":["g4"],"text":unfinished}
            ]}),
        )
        .unwrap();
    assert_eq!(
        state.render_primary(),
        input,
        "advice is not a selected rewrite"
    );
    assert!(
        groups
            .iter()
            .filter(|g| [2, 4].contains(&g.id))
            .all(|g| state.drop_authorized(g))
    );
    state.remove(&[2, 4], &groups);
    let rendered = state.render_primary_with_ids();
    assert_eq!(
        rendered.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![Some(1), None, None, Some(3), None, None, Some(7)]
    );
    assert_eq!(rendered[1].1, omission_notice());
    assert_eq!(rendered[4].1, omission_notice());
    for (index, text) in [(2, outcome), (5, unfinished)] {
        let content = rendered[index].1["content"].as_str().unwrap();
        let data: Value = serde_json::from_str(content.split_once(": ").unwrap().1).unwrap();
        assert_eq!(data["text"], text);
    }
    let before = state.render_primary();
    assert_eq!(before, state.render_primary());
    let mut restored: Session =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(restored.render_primary(), before);
    restored.observe_groups(&restored.groups());
    let reviewer = json!(restored.shadow_input());
    let reviewer = reviewer.to_string();
    assert!(!reviewer.contains("synthetic-opaque"));
    assert_eq!(reviewer.matches(outcome).count(), 1);
    assert_eq!(reviewer.matches(unfinished).count(), 1);
    assert_eq!(restored.render_primary(), before);
}

#[test]
fn omission_coalesces_removed_runs_even_with_memories_and_repeated_rewrites() {
    let mut state = Session::default();
    let input = (0..6)
        .map(|i| json!({"role":"user","content":format!("source {i}")}))
        .collect::<Vec<_>>();
    state.ingest(&input).unwrap();
    for item in &mut state.history {
        item.exposed = true;
    }
    let groups = state.groups();
    state
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[],"memories":[
                {"source_ids":["g1"],"text":"first fact"},
                {"source_ids":["g2"],"text":"second fact"}
            ]}),
        )
        .unwrap();
    state.remove(&[1, 2, 4], &groups);
    let first = state.render_primary();
    assert_eq!(first.iter().filter(|v| **v == omission_notice()).count(), 2);
    assert_eq!(first[0], omission_notice());
    assert_eq!(first[3], input[2]);
    assert_eq!(first[4], omission_notice());
    state.remove(&[3], &state.groups());
    let joined = state.render_primary();
    assert_eq!(
        joined.iter().filter(|v| **v == omission_notice()).count(),
        1
    );
    assert_eq!(joined[0], omission_notice());
    assert_eq!(&joined[joined.len() - 2..], &input[4..]);
    state.remove(&[5, 6], &state.groups());
    let all = state.render_primary();
    assert_eq!(all.iter().filter(|v| **v == omission_notice()).count(), 1);
    assert_eq!(
        all.len(),
        3,
        "full omission retains one notice and two facts"
    );
    let restored: Session = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
    assert_eq!(restored.render_primary(), all);
}

#[test]
fn omission_rejected_replacement_preserves_source_without_requiring_every_drop_to_have_memory() {
    for malformed in [false, true] {
        let (mut state, input) = omission_fixture();
        let groups = state.groups();
        state.apply_advice_with_memory_admission(&groups, &json!({"protected":[],"removable":["g2","g4"],"memories":[
            {"source_ids":["g4"],"text":if malformed { "" } else { "Migration remains unfinished; next fixture B." }}
        ]}), |_| false).unwrap();
        assert!(state.memories.is_empty());
        assert_eq!(state.review_feedback["rejected_count"], 1);
        let work = groups.iter().find(|g| g.id == 4).unwrap();
        assert!(
            !state.drop_authorized(work),
            "rejected replacement must not release unique work evidence"
        );
        assert!(
            state.drop_authorized(groups.iter().find(|g| g.id == 2).unwrap()),
            "unrelated explicit drops do not require a memory"
        );
        let restored: Session =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        assert!(!restored.drop_authorized(work));
        assert_eq!(restored.render_primary(), input);
    }
}
