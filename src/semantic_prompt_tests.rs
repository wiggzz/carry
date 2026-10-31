// Block 1 semantic-prompt boundary. This is a tests-first tracer, not a second
// accounting owner. Compiled execution must precede changing that owner.
fn semantic_marker_body() -> Value {
    json!({"model":"gpt-6.1-sol","max_output_tokens":1,
        "instructions":"fixed instructions",
        "tools":[{"type":"function","name":"tool","parameters":{
            "type":"object","properties":{"prompt_cache_breakpoint":{"type":"string"}}}}],
        "input":[
            {"role":"developer","content":[{"type":"input_text","text":"fixed developer"}]},
            {"type":"message","role":"assistant","id":"msg-1","signature":"signed-1",
             "metadata":{"prompt_cache_breakpoint":{"caller":"keep this"}},
             "content":[{"type":"input_text","text":"old history"}]},
            {"type":"function_call_output","call_id":"call-1",
             "output":{"prompt_cache_breakpoint":{"caller":"tool output is not transport"}}},
            {"role":"user","content":[{"type":"input_text","text":"current request"}]}]})
}

fn mark_semantic_body(request: &mut Value, ttl: &str) {
    let marker = json!({"mode":"explicit","ttl":ttl});
    request["prompt_cache_breakpoint"] = marker.clone();
    for index in [0, 1, 3] {
        request["input"][index]["prompt_cache_breakpoint"] = marker.clone();
        request["input"][index]["content"][0]["prompt_cache_breakpoint"] = marker.clone();
    }
}

#[test]
fn semantic_prompt_projection_and_retained_quantities_ignore_only_transport_markers() {
    use crate::savings::{fixed_prompt_tokens, prompt_projection, protected_prompt_tokens};
    let canonical = semantic_marker_body();
    let expected = prompt_projection(&canonical).unwrap();
    let protected = protected_prompt_tokens(&canonical).unwrap();
    let fixed = fixed_prompt_tokens(&canonical).unwrap();
    let mut marked = canonical.clone();
    mark_semantic_body(&mut marked, "30m");
    let mut changes = Vec::new();
    // These are executable semantic guards, not prompt-wording assertions.
    for (pointer, value) in [
        (
            "/input/1/content/0/text",
            json!("genuinely changed history"),
        ),
        ("/input/1/role", json!("user")),
        ("/input/1/id", json!("msg-2")),
        ("/input/1/signature", json!("signed-2")),
        (
            "/input/1/metadata/prompt_cache_breakpoint/caller",
            json!("new caller data"),
        ),
        (
            "/input/2/output/prompt_cache_breakpoint/caller",
            json!("new tool output"),
        ),
        (
            "/tools/0/parameters/properties/prompt_cache_breakpoint/type",
            json!("number"),
        ),
    ] {
        let mut changed = marked.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert_ne!(
            prompt_projection(&changed).unwrap().0,
            prompt_projection(&marked).unwrap().0,
            "semantic content must remain fenced: {pointer}"
        );
    }
    for ttl in ["30m", "12h"] {
        mark_semantic_body(&mut marked, ttl);
        if prompt_projection(&marked).as_ref() != Some(&expected)
            || protected_prompt_tokens(&marked) != Some(protected)
            || fixed_prompt_tokens(&marked) != Some(fixed)
        {
            changes.push(ttl);
        }
    }
    assert!(
        changes.is_empty(),
        "SEMANTIC_PROMPT_OWNER: transport markers changed prompt identity/quantities at {changes:?}"
    );
}

fn semantic_markers_preserve_summarized_growth(initialized: bool) {
    let mut request = semantic_marker_body();
    let mut t = Trajectory::default();
    if !initialized {
        t.primary(
            &request,
            &terminal(10000, 0, 0, 0),
            "semantic-history",
            99,
            true,
        );
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"grow"}));
    }
    for (step, input) in [300000, 300100, 300200].into_iter().enumerate() {
        if step > 0 {
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("seed {step}")}));
        }
        t.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "semantic-history",
            100 + step as u64,
            true,
        );
    }
    t.reviewer(&request, &terminal(100, 0, 0, 10), true);
    let before = t.snapshot();
    assert_eq!(
        (
            t.history.retired,
            t.history.summary,
            t.history.last_traditional_input
        ),
        (280000, 1000, 21200)
    );
    assert_eq!(before.initial_compactions, u64::from(initialized));
    assert_eq!(before.traditional_compactions, u64::from(!initialized));
    assert_eq!(before.summary_cost > 0.0, !initialized);
    assert!(before.reviewer_cost > 0.0);
    let mut differences = Vec::new();
    let mut paid = before.observed_primary_cost;
    for (step, ttl) in ["30m", "12h"].into_iter().enumerate() {
        t = serde_json::from_value(state(&t)).unwrap();
        mark_semantic_body(&mut request, ttl);
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":format!("continue {step}")}));
        let input = 300300 + step as u64 * 100;
        t.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "semantic-history",
            103 + step as u64,
            true,
        );
        let after = t.snapshot();
        paid += input as f64 * 4.0 / 1e6;
        assert!((after.observed_primary_cost - paid).abs() < 1e-12);
        assert_eq!(after.unavailable_primary_cost_calls, 0);
        assert_eq!(after.coverage.missing_usage, 0);
        assert_eq!(
            after.coverage.primary_responses,
            before.coverage.primary_responses + step as u64 + 1
        );
        assert_eq!(after.observed_reviewer_cost, before.observed_reviewer_cost);
        assert_eq!(after.reviewer_cost, before.reviewer_cost);
        let quantity = (
            t.history.retired,
            t.history.summary,
            t.history.last_traditional_input,
        );
        let expected = (280000, 1000, 21300 + step as u64 * 100);
        if quantity != expected
            || after.initial_compactions != before.initial_compactions
            || after.traditional_compactions != before.traditional_compactions
            || after.summary_cost != before.summary_cost
        {
            differences.push(format!(
                "{ttl}: {quantity:?} vs {expected:?}; initial {}, paid {}, summary {}",
                after.initial_compactions, after.traditional_compactions, after.summary_cost
            ));
        }
        // An explicit-extent forecast can be unknown. Never substitute native
        // hits, demand a new tariff, or change frozen past money to make it known.
    }
    assert!(
        differences.is_empty(),
        "SEMANTIC_HISTORY_GROWTH: {differences:?}"
    );
}

#[test]
fn semantic_markers_preserve_free_initialized_growth_across_restart() {
    semantic_markers_preserve_summarized_growth(true);
}

#[test]
fn semantic_markers_preserve_paid_summary_growth_across_restart() {
    semantic_markers_preserve_summarized_growth(false);
}

fn legacy_raw_marker_cursor_continuation(exact: bool) {
    use sha2::{Digest, Sha256};
    let mut request = semantic_marker_body();
    mark_semantic_body(&mut request, "30m");
    let mut t = Trajectory::default();
    t.primary(
        &request,
        &terminal(300000, 0, 0, 0),
        "raw-cursor",
        100,
        true,
    );
    t.reviewer(&request, &terminal(100, 0, 0, 10), true);
    let before = t.snapshot();
    assert_eq!(
        (
            t.history.retired,
            t.history.summary,
            t.history.last_traditional_input
        ),
        (280000, 1000, 21000)
    );
    assert_eq!(before.initial_compactions, 1);
    assert!(before.observed_primary_cost > 0.0 && before.observed_reviewer_cost > 0.0);
    let mut saved = state(&t);
    // Exact historical shape: no new cursor-schema field can be inherited from
    // this candidate's serializer. The digest really names the old raw items.
    saved["history_continuity"]["cursor"] = json!({
        "prefix":format!("{:x}", Sha256::digest(serde_json::to_vec(&request["input"]).unwrap())),
        "items":request["input"].as_array().unwrap().len(), "input":300000, "at":100});
    let mut old_cursor = saved["history_continuity"].clone();
    // The old serializer omitted this field; the new decoder emits its exact
    // RawV1 default even when proof is unavailable. Normalize ONLY that known
    // representation difference. Contract/prefix/items/input/time stay exact.
    old_cursor["cursor"]["basis"] = json!("RawV1");
    let mut differences = Vec::new();
    let mut paid = before.observed_primary_cost;
    for step in 0..2_u64 {
        t = serde_json::from_value(saved).unwrap();
        if !exact {
            mark_semantic_body(&mut request, "12h");
        }
        request["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":format!("raw continuation {step}")}));
        let input = 300100 + step * 100;
        t.primary(
            &request,
            &terminal(input, 0, 0, 0),
            "raw-cursor",
            101 + step,
            true,
        );
        let after = t.snapshot();
        paid += input as f64 * 4.0 / 1e6;
        assert!((after.observed_primary_cost - paid).abs() < 1e-12);
        assert_eq!(after.unavailable_primary_cost_calls, 0);
        assert_eq!(
            after.coverage.primary_responses,
            before.coverage.primary_responses + step + 1
        );
        assert_eq!(after.observed_reviewer_cost, before.observed_reviewer_cost);
        assert_eq!(after.reviewer_cost, before.reviewer_cost);
        assert_eq!(after.carry_cost, before.carry_cost);
        assert_eq!(after.traditional_cost, before.traditional_cost);
        let quantity = (
            t.history.retired,
            t.history.summary,
            t.history.last_traditional_input,
        );
        let expected = (280000, 1000, if exact { 21100 + step * 100 } else { 21000 });
        if quantity != expected
            || after.initial_compactions != 1
            || after.traditional_compactions != 0
            || after.summary_cost != before.summary_cost
            || (!exact && state(&t)["history_continuity"] != old_cursor)
        {
            differences.push(format!(
                "step {step}: {quantity:?} vs {expected:?}; initialization {}",
                after.initial_compactions
            ));
        }
        saved = state(&t);
    }
    assert!(
        differences.is_empty(),
        "LEGACY_RAW_SEMANTIC_PROOF: exact={exact}: {differences:?}"
    );
}

#[test]
fn legacy_raw_marker_cursor_exact_proof_retains_native_growth() {
    legacy_raw_marker_cursor_continuation(true);
}

#[test]
fn legacy_raw_marker_cursor_changed_policy_stays_read_only_not_reinitialized() {
    legacy_raw_marker_cursor_continuation(false);
}

// Review4430659252: a cursor-format upgrade is not a history edit. This stages
// the predecessor's exact raw cursor shape, not a previous executable. Paired
// current-format controls exercise the same real coordinator and restarted
// continuations. Noodle's actual old/new CLI probe remains separate evidence.
fn legacy_raw_cursor_preserves_recorded_lifetime_proof(proof: Option<bool>) {
    use sha2::{Digest, Sha256};
    let request = semantic_marker_body();
    let mut seed = Trajectory::default();
    seed.primary(
        &request,
        &terminal(2000, 0, 0, 0),
        "raw-lifetime-proof",
        100,
        true,
    );
    let seed_snapshot = seed.snapshot();
    assert!((seed_snapshot.observed_primary_cost - 0.004).abs() < 1e-12);
    assert_eq!(
        seed_snapshot.carry_cost,
        seed_snapshot.observed_primary_cost
    );
    assert_eq!(seed_snapshot.traditional_cost, seed_snapshot.carry_cost);
    assert_eq!(state(&seed)["shared_primary_history"], json!(true));
    let mut controls = Vec::new();
    let mut differences = Vec::new();
    for raw in [false, true] {
        let mut saved = state(&seed);
        match proof {
            Some(value) => saved["shared_primary_history"] = json!(value),
            None => {
                saved
                    .as_object_mut()
                    .unwrap()
                    .remove("shared_primary_history");
            }
        }
        if raw {
            saved["history_continuity"]["cursor"] = json!({
                "prefix":format!("{:x}", Sha256::digest(serde_json::to_vec(&request["input"]).unwrap())),
                "items":request["input"].as_array().unwrap().len(), "input":2000, "at":100});
        }
        let mut request = request.clone();
        for step in 0..2_usize {
            let mut t: Trajectory = serde_json::from_value(saved).unwrap();
            request["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"user","content":format!("same append {step}")}));
            let frozen = serde_json::to_value(t.snapshot()).unwrap();
            t.load_receipt_history(&request, "raw-lifetime-proof", None, std::iter::empty());
            assert_eq!(serde_json::to_value(t.snapshot()).unwrap(), frozen);
            let loaded = state(&t);
            t.load_receipt_history(&request, "raw-lifetime-proof", None, std::iter::empty());
            assert_eq!(state(&t), loaded, "cursor conversion must be idempotent");
            assert_eq!(
                state(&t)["history_continuity"]["cursor"]["basis"],
                json!("SemanticV1")
            );
            t.primary(
                &request,
                &terminal(2100 + step as u64 * 100, 0, 0, 0),
                "raw-lifetime-proof",
                101 + step as u64,
                true,
            );
            let after = t.snapshot();
            let actual = if step == 0 { 0.0082 } else { 0.0126 };
            // Every paid/quantity/count guard executes before the new semantic
            // failure, so a changed proof cannot conceal a broken fixture.
            assert!((after.observed_primary_cost - actual).abs() < 1e-12);
            assert_eq!(after.carry_cost, after.observed_primary_cost);
            assert_eq!(after.coverage.primary_responses, 2 + step as u64);
            assert_eq!(after.matched_primary_responses, 2 + step as u64);
            assert_eq!(after.unavailable_primary_cost_calls, 0);
            assert_eq!(after.excluded_unpriced_calls, 0);
            assert_eq!(after.coverage.missing_usage, 0);
            assert_eq!(after.coverage.unmodeled_calls, 0);
            assert_eq!(after.observed_reviewer_cost, 0.0);
            assert_eq!(after.reviewer_cost, 0.0);
            assert_eq!(after.initial_compactions, 0);
            assert_eq!(after.traditional_compactions, 0);
            assert_eq!(after.summary_cost, 0.0);
            assert_eq!((t.history.retired, t.history.summary), (0, 0));
            assert_eq!(t.history.last_traditional_input, 2100 + step as u64 * 100);
            saved = state(&t);
            let view = json!({"proof":saved["shared_primary_history"], "estimate":after});
            if !raw {
                assert_eq!(
                    saved["shared_primary_history"],
                    json!(proof.unwrap_or(false))
                );
                if proof == Some(true) {
                    assert!((after.traditional_cost - actual).abs() < 1e-12);
                } else {
                    // Exact cursor agreement must NOT promote missing/false
                    // lifetime proof or copy the observed primary onto it.
                    assert!((after.traditional_cost - actual).abs() > 1e-6);
                }
                controls.push(view);
            } else if view != controls[step] {
                differences.push(format!(
                    "step {step}: proof {} vs {:?}, traditional {} vs {}",
                    saved["shared_primary_history"],
                    proof.unwrap_or(false),
                    after.traditional_cost,
                    controls[step]["estimate"]["traditional_cost"]
                ));
            }
        }
    }
    assert!(
        differences.is_empty(),
        "RAW_CURSOR_LIFETIME_PROOF: {differences:?}"
    );
}

#[test]
fn legacy_raw_cursor_preserves_true_lifetime_proof_across_restart() {
    legacy_raw_cursor_preserves_recorded_lifetime_proof(Some(true));
}

#[test]
fn legacy_raw_cursor_does_not_promote_false_lifetime_proof_across_restart() {
    legacy_raw_cursor_preserves_recorded_lifetime_proof(Some(false));
}

#[test]
fn legacy_raw_cursor_does_not_infer_missing_lifetime_proof_across_restart() {
    legacy_raw_cursor_preserves_recorded_lifetime_proof(None);
}
