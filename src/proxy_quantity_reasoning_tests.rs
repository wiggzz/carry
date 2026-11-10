use super::*;
use serde_json::json;

fn neutral_tokens(m: &InputMeasurements, target: &Value, source: &Value) -> u64 {
    let baseline = m.forecast_estimate(source, "route", None).unwrap();
    let restoration = m
        .accounting_restoration(target, source, "route", None)
        .unwrap();
    u64::try_from(baseline.tokens as i128 + restoration.tokens).unwrap()
}

fn fixture() -> (InputMeasurements, Value, Value) {
    let source = json!({"model":"gpt-6.1-sol","instructions":"fixed", "input":[
        {"role":"user","content":"visible history ".repeat(100)},
        {"type":"reasoning","summary":[],"encrypted_content":"synthetic-a".repeat(1000)},
        {"type":"reasoning","summary":[],"encrypted_content":"synthetic-b".repeat(1000)}]});
    let mut measured = InputMeasurements::default();
    measured.observe(&source, &json!({"input_tokens":10000}), "route", None, 100);
    let mut target = source.clone();
    target["input"].as_array_mut().unwrap().remove(1);
    (measured, source, target)
}

#[test]
fn filtered_receipt_restart_and_fresh_append_allow_further_reasoning_removal() {
    let (mut measured, source, target) = fixture();
    assert!(
        measured
            .projected_estimate(&target, &source, "route", None)
            .is_some()
    );
    measured.observe(&target, &json!({"input_tokens":6000}), "route", None, 101);
    let saved = serde_json::to_string(&measured).unwrap();
    assert!(!saved.contains("synthetic") && !saved.contains("encrypted_content"));
    let mut measured: InputMeasurements = serde_json::from_str(&saved).unwrap();
    let mut appended = target.clone();
    appended["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"continue"}));
    let baseline = measured.predict(&appended, "route", None).unwrap();
    let mut next = appended.clone();
    next["input"].as_array_mut().unwrap().remove(1);
    let estimate = measured
        .projected_estimate(&next, &appended, "route", None)
        .unwrap();
    assert_eq!(estimate.tokens, neutral_tokens(&measured, &next, &appended));
    assert!(estimate.tokens > 0 && estimate.tokens < baseline.tokens);
    assert_eq!(
        estimate.basis,
        EstimateBasis::ReceiptAnchoredReasoningForecast
    );
    // Reintroduced opaque members may be estimated, never promoted to native proof.
    assert_eq!(
        measured
            .projected_estimate(&source, &target, "route", None)
            .unwrap()
            .capacity_tokens(),
        None
    );
    assert!(
        measured
            .restoration(&source, &target, "route", None)
            .is_none()
    );
    measured.observe(&appended, &json!({"input_tokens":6020}), "route", None, 102);
    assert!(
        measured
            .projected_estimate(&next, &appended, "route", None)
            .is_some()
    );
}

#[test]
fn partially_retained_measured_block_recalibrates_after_filtered_receipt() {
    let (_, source, target) = fixture();
    let mut initial = source.clone();
    initial["input"].as_array_mut().unwrap().truncate(1);
    let mut measured = InputMeasurements::default();
    measured.observe(&initial, &json!({"input_tokens":1000}), "route", None, 99);
    measured.observe(&source, &json!({"input_tokens":10000}), "route", None, 100);
    let first = measured
        .projected_estimate(&target, &source, "route", None)
        .unwrap();
    assert_eq!(first.tokens, neutral_tokens(&measured, &target, &source));
    assert!(first.tokens > 1000 && first.tokens < 10000);
    measured.observe(&target, &json!({"input_tokens":6000}), "route", None, 101);
    let restored = measured
        .projected_estimate(&source, &target, "route", None)
        .unwrap();
    assert_eq!(restored.tokens, neutral_tokens(&measured, &source, &target));
    assert_eq!(restored.capacity_tokens(), None);
    assert!(
        measured
            .restoration(&source, &target, "route", None)
            .is_none()
    );
    let measured: InputMeasurements =
        serde_json::from_value(serde_json::to_value(measured).unwrap()).unwrap();
    let estimate = measured
        .projected_estimate(&initial, &target, "route", None)
        .expect("incomplete older block must not permanently stall subsequent calibrated removals");
    assert_eq!(
        estimate.tokens,
        neutral_tokens(&measured, &initial, &target)
    );
    assert!(estimate.tokens > 0 && estimate.tokens < 6000);
}

#[test]
fn conflicting_native_counts_disable_reasoning_calibration_but_keep_actual_receipt() {
    let (mut measured, source, target) = fixture();
    measured.observe(&source, &json!({"input_tokens":12000}), "route", None, 101);
    assert_eq!(
        measured.predict(&source, "route", None).unwrap().tokens,
        12000
    );
    assert!(
        measured
            .projected_estimate(&target, &source, "route", None)
            .is_none(),
        "incompatible repeated whole-prompt counts cannot recalibrate guessed removals"
    );
    let measured: InputMeasurements =
        serde_json::from_value(serde_json::to_value(measured).unwrap()).unwrap();
    assert!(
        measured
            .projected_estimate(&target, &source, "route", None)
            .is_none()
    );
}

#[test]
fn unsupported_modalities_cannot_donate_a_whole_prompt_residual_to_reasoning() {
    for opaque in [
        json!({"role":"user","content":[{"type":"input_image","image_url":"synthetic"}]}),
        json!({"role":"user","content":[{"type":"input_audio","input_audio":{"data":"synthetic","format":"wav"}}]}),
        json!({"role":"user","content":[{"type":"input_file","file_id":"synthetic"}]}),
        json!({"type":"compaction","encrypted_content":"synthetic"}),
    ] {
        let (_, mut source, _) = fixture();
        source["input"].as_array_mut().unwrap().push(opaque);
        let mut measured = InputMeasurements::default();
        measured.observe(&source, &json!({"input_tokens":10000}), "route", None, 100);
        let mut target = source.clone();
        target["input"].as_array_mut().unwrap().remove(1);
        assert!(
            measured
                .projected_estimate(&target, &source, "route", None)
                .is_none()
        );
        target = source.clone();
        target["input"].as_array_mut().unwrap().pop();
        assert!(
            measured
                .projected_estimate(&target, &source, "route", None)
                .is_none()
        );
    }
}

#[test]
fn forecast_requires_current_compatible_evidence_and_consistent_budget() {
    let (mut measured, source, target) = fixture();
    assert!(
        InputMeasurements::default()
            .projected_estimate(&target, &source, "route", None)
            .is_none()
    );
    assert!(
        measured
            .projected_estimate(&target, &source, "other", None)
            .is_none()
    );
    for field in ["model", "instructions", "tools", "reasoning"] {
        let mut changed = source.clone();
        changed[field] = json!("different");
        let mut changed_target = target.clone();
        changed_target[field] = json!("different");
        assert!(
            measured
                .projected_estimate(&changed_target, &changed, "route", None)
                .is_none()
        );
    }
    measured.observe(&target, &json!({"input_tokens":6000}), "route", None, 101);
    measured.observe(&source, &json!({"input_tokens":10000}), "route", None, 99);
    assert!(
        measured
            .projected_estimate(&target, &source, "route", None)
            .is_some()
    ); // target's actual receipt wins
    assert_eq!(
        measured
            .projected_estimate(&source, &target, "route", None)
            .unwrap()
            .capacity_tokens(),
        None
    );
    let (_, source, target) = fixture();
    for input in [json!(0), json!(1), json!(-1), json!(1.5), Value::Null] {
        let mut measured = InputMeasurements::default();
        measured.observe(&source, &json!({"input_tokens":input}), "route", None, 100);
        assert!(
            measured
                .projected_estimate(&target, &source, "route", None)
                .is_none()
        );
    }
}

#[test]
fn added_visible_replacement_uses_shared_local_section_quantity() {
    let (measured, source, mut target) = fixture();
    let removed = measured
        .projected_estimate(&target, &source, "route", None)
        .unwrap();
    let addition = json!({"role":"assistant","content":"new retained memory ".repeat(30)});
    let local = measured.visible_text_tokens(&addition);
    target["input"].as_array_mut().unwrap().push(addition);
    let replaced = measured
        .projected_estimate(&target, &source, "route", None)
        .unwrap();
    assert_eq!(
        replaced.tokens - removed.tokens,
        local,
        "the same local section units belong to planner and accounting"
    );
}

#[test]
fn fixed_direction_floor_cannot_be_allocated_to_unresolved_reasoning() {
    let (_, mut source, _) = fixture();
    source["input"].as_array_mut().unwrap().insert(
        0,
        json!({"role":"developer","content":"fixed direction ".repeat(1000)}),
    );
    let mut target = source.clone();
    target["input"].as_array_mut().unwrap().remove(2);
    let mut measured = InputMeasurements::default();
    measured.observe(&source, &json!({"input_tokens":100}), "route", None, 100);
    assert!(
        measured
            .projected_estimate(&target, &source, "route", None)
            .is_none(),
        "a native total below the fixed direction floor is an inconsistent calibration budget"
    );
}

#[test]
fn known_native_blocks_are_not_double_counted_in_the_unresolved_pool() {
    let (mut measured, source, target) = fixture();
    let old = measured
        .projected_estimate(&target, &source, "route", None)
        .unwrap();
    let image =
        json!({"role":"user","content":[{"type":"input_image","image_url":"synthetic-native"}]});
    let mut appended = source.clone();
    appended["input"]
        .as_array_mut()
        .unwrap()
        .push(image.clone());
    measured.observe(
        &appended,
        &json!({"input_tokens":60000}),
        "route",
        None,
        101,
    );
    let mut projected = target.clone();
    projected["input"].as_array_mut().unwrap().push(image);
    assert_eq!(
        measured
            .projected_estimate(&projected, &appended, "route", None)
            .unwrap()
            .tokens,
        old.tokens + 50000
    );
}

#[test]
fn reordered_restored_native_block_cannot_borrow_another_reasoning_forecast() {
    let (_, expanded, _) = fixture();
    let mut initial = expanded.clone();
    initial["input"].as_array_mut().unwrap().truncate(1);
    let mut measured = InputMeasurements::default();
    measured.observe(&initial, &json!({"input_tokens":1000}), "route", None, 99);
    measured.observe(
        &expanded,
        &json!({"input_tokens":10000}),
        "route",
        None,
        100,
    );
    let mut source = initial.clone();
    source["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"reasoning","encrypted_content":"synthetic-new".repeat(2000)}));
    measured.observe(&source, &json!({"input_tokens":20000}), "route", None, 101);
    let mut reordered = expanded.clone();
    reordered["input"].as_array_mut().unwrap().swap(1, 2);
    assert!(measured.predict(&reordered, "route", None).is_none());
    let estimated = measured
        .projected_estimate(&reordered, &source, "route", None)
        .unwrap();
    assert_eq!(
        estimated.capacity_tokens(),
        None,
        "reordered members cannot become exact native/capacity proof"
    );
}

#[test]
fn partial_reasoning_allocation_scales_with_actual_native_block_totals() {
    let (_, expanded, target) = fixture();
    let mut initial = expanded.clone();
    initial["input"].as_array_mut().unwrap().truncate(1);
    let mut losses = Vec::new();
    for delta in [4000u64, 8000] {
        let mut measured = InputMeasurements::default();
        measured.observe(&initial, &json!({"input_tokens":1000}), "route", None, 99);
        measured.observe(
            &expanded,
            &json!({"input_tokens":1000+delta}),
            "route",
            None,
            100,
        );
        let forecast = measured
            .projected_estimate(&target, &expanded, "route", None)
            .unwrap();
        losses.push(1000 + delta - forecast.tokens);
        assert_eq!(
            measured
                .projected_estimate(&initial, &expanded, "route", None)
                .unwrap()
                .tokens,
            1000
        );
    }
    assert!(losses[0] > 0 && (losses[1] == losses[0] * 2 || losses[1] == losses[0] * 2 + 1));
}

fn repeated_visible_fixture(shared_across_blocks: bool) -> (InputMeasurements, Value, Value) {
    let u = json!({"role":"user","content":"initial"});
    let v = json!({"role":"assistant","content":"identical visible sibling"});
    let v2 = if shared_across_blocks {
        v.clone()
    } else {
        json!({"role":"assistant","content":"other visible sibling"})
    };
    let r1 = json!({"type":"reasoning","encrypted_content":"synthetic-r1".repeat(1000)});
    let r2 = json!({"type":"reasoning","encrypted_content":"synthetic-r2".repeat(1000)});
    let body = |input: Vec<Value>| json!({"model":"gpt-6.1-sol","input":input});
    let initial = body(vec![u.clone()]);
    let first = body(vec![u.clone(), r1.clone(), v.clone(), v.clone()]);
    let source = body(vec![u.clone(), r1, v.clone(), v.clone(), r2, v2.clone()]);
    let target = body(vec![u, v, v2]);
    let mut measured = InputMeasurements::default();
    for (at, body, input) in [
        (100, &initial, 1000),
        (101, &first, 10000),
        (102, &source, 12000),
    ] {
        measured.observe(body, &json!({"input_tokens":input}), "route", None, at);
    }
    (measured, source, target)
}

#[test]
fn repeated_visible_members_cannot_turn_partial_blocks_into_whole_removals() {
    let (measured, source, target) = repeated_visible_fixture(false);
    let estimate = measured
        .projected_estimate(&target, &source, "route", None)
        .expect("distinct native reasoning with duplicate visible siblings remains forecastable");
    assert_eq!(
        estimate.basis,
        EstimateBasis::ReceiptAnchoredReasoningForecast
    );
    assert_eq!(estimate.tokens, neutral_tokens(&measured, &target, &source));
    assert!(
        estimate.tokens > 1000 && estimate.tokens < 12000,
        "retained visible occurrences keep their own quantities"
    );
    assert_eq!(estimate.capacity_tokens(), None);
}

#[test]
fn repeated_visible_occurrences_are_not_reused_across_native_blocks() {
    let (measured, source, target) = repeated_visible_fixture(true);
    let estimate = measured
        .projected_estimate(&target, &source, "route", None)
        .expect("the two retained V occurrences belong to the first block, not the second");
    assert_eq!(
        estimate.basis,
        EstimateBasis::ReceiptAnchoredReasoningForecast
    );
    assert_eq!(estimate.tokens, neutral_tokens(&measured, &target, &source));
    assert!(estimate.tokens > 1000 && estimate.tokens < 12000);
    assert_eq!(estimate.capacity_tokens(), None);
    // The entire second block still has its exact native quantity, even
    // though its visible identity is shared with the retained first block.
    let mut whole = source.clone();
    whole["input"].as_array_mut().unwrap().truncate(4);
    assert_eq!(
        measured.predict(&whole, "route", None).unwrap().tokens,
        10000
    );
}

#[test]
fn repeated_visible_partial_additions_share_native_residual_attribution() {
    let (mut measured, target, mut source) = repeated_visible_fixture(false);
    let retained = source.clone();
    source["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"reasoning","encrypted_content":"synthetic-new".repeat(1000)}));
    measured.observe(&source, &json!({"input_tokens":20000}), "route", None, 103);
    let removed = measured
        .projected_estimate(&retained, &source, "route", None)
        .unwrap();
    let estimate = measured
        .projected_estimate(&target, &source, "route", None)
        .unwrap();
    assert_eq!(
        estimate.basis,
        EstimateBasis::ReceiptAnchoredReasoningForecast
    );
    let added = estimate.tokens.checked_sub(removed.tokens).unwrap();
    assert_eq!(estimate.tokens, neutral_tokens(&measured, &target, &source));
    assert!(
        added > 0 && added < 11000,
        "retained visible siblings cannot donate their local units to hidden reasoning"
    );
    assert_eq!(estimate.capacity_tokens(), None);
}

#[test]
fn heuristic_reporting_never_initiates_full_history_tokenization() {
    use crate::prompt_quantity::{reset_tokenized_bytes, tokenized_bytes};
    let (measured, source, target) = fixture();
    reset_tokenized_bytes();
    assert!(measured.known_estimate(&target, "route", None).is_none());
    assert_eq!(tokenized_bytes(), 0);
    measured
        .projected_estimate(&target, &source, "route", None)
        .unwrap();
    reset_tokenized_bytes();
    assert!(measured.known_estimate(&target, "route", None).is_none());
    assert_eq!(tokenized_bytes(), 0);
}

fn initial_occurrence_pool() -> (InputMeasurements, Value) {
    let visible = json!({"role":"assistant","content":"repeated visible evidence ".repeat(30)});
    let source = json!({"model":"gpt-6.1-sol","instructions":"fixed", "input":[
        {"role":"user","content":"goal"},
        {"type":"reasoning","summary":[],"encrypted_content":"synthetic-old-a".repeat(1000)},
        visible.clone(), visible,
        {"type":"reasoning","summary":[],"encrypted_content":"synthetic-old-b".repeat(1000)}]});
    let mut measured = InputMeasurements::default();
    measured.observe(&source, &json!({"input_tokens":10000}), "route", None, 100);
    (measured, source)
}

#[test]
fn initial_pool_counts_repeated_visible_occurrences_without_reusing_removal_credit() {
    let (measured, anchor) = initial_occurrence_pool();
    assert!(measured.blocks.is_empty());
    let measured: InputMeasurements =
        serde_json::from_value(serde_json::to_value(measured).unwrap()).unwrap();
    let mut source = anchor.clone();
    source["input"].as_array_mut().unwrap().push(json!({
        "type":"reasoning","summary":[],"encrypted_content":"synthetic-fresh".repeat(1000)}));
    let baseline = measured.forecast_estimate(&source, "route", None).unwrap();
    assert_eq!(
        baseline.basis,
        EstimateBasis::ReceiptAnchoredReasoningForecast
    );
    assert!(baseline.tokens > 10000);
    assert!(measured.predict(&source, "route", None).is_none());

    // Independent neutral residual oracle: local visible occurrences are
    // counted once each; the remaining native budget is shared equally by
    // the two opaque members, never weighted by ciphertext bytes.
    let local = anchor["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| measured.visible_text_tokens(value))
        .collect::<Vec<_>>();
    assert_eq!(local[2], local[3]);
    let budget = 10000 - measured.visible_text_tokens(&crate::savings::prompt_overhead(&source));
    let residual = budget - local.iter().sum::<u64>();
    let reasoning_share = local[1] + residual / 2 + residual % 2;
    let available = anchor["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| (identity(value), value))
        .collect();
    let (pool, pool_budget) = measured.unmeasured_pool(&available, &source).unwrap();
    assert_eq!(
        pool.len(),
        5,
        "pool cardinality is occurrences, not unique hashes"
    );
    assert_eq!(
        pool.iter()
            .filter(|id| **id == identity(&anchor["input"][2]))
            .count(),
        2
    );
    assert_eq!(pool_budget, budget);
    let mut forecasts = Vec::new();
    for removed_copies in 0..=2 {
        let mut target = source.clone();
        target["input"].as_array_mut().unwrap().remove(1);
        for _ in 0..removed_copies {
            target["input"].as_array_mut().unwrap().remove(1);
        }
        let projected = measured
            .projected_estimate(&target, &source, "route", None)
            .expect("unmeasured visible occurrences cannot veto calibrated old reasoning removal");
        let credit = reasoning_share + removed_copies as u64 * local[2];
        assert_eq!(projected.tokens, baseline.tokens - credit);
        assert_eq!(
            projected.basis,
            EstimateBasis::ReceiptAnchoredReasoningForecast
        );
        assert!(measured.predict(&target, "route", None).is_none());
        assert!(
            measured
                .restoration(&source, &target, "route", None)
                .is_none()
        );
        forecasts.push(projected.tokens);
    }
    assert!(forecasts.windows(2).all(|pair| pair[0] > pair[1]));
}

#[test]
fn repeated_visible_pool_preserves_complete_native_block_budget() {
    let (mut measured, source) = initial_occurrence_pool();
    let mut target = source.clone();
    target["input"].as_array_mut().unwrap().remove(1);
    let original = measured
        .projected_estimate(&target, &source, "route", None)
        .unwrap();
    let image =
        json!({"role":"user","content":[{"type":"input_image","image_url":"synthetic-native"}]});
    let mut appended = source.clone();
    appended["input"]
        .as_array_mut()
        .unwrap()
        .push(image.clone());
    measured.observe(
        &appended,
        &json!({"input_tokens":60000}),
        "route",
        None,
        101,
    );
    assert_eq!(measured.blocks.len(), 1);
    assert_eq!(measured.blocks[0].input, 50000);
    target["input"].as_array_mut().unwrap().push(image);
    assert_eq!(
        measured
            .projected_estimate(&target, &appended, "route", None)
            .unwrap()
            .tokens,
        original.tokens + 50000
    );
}

#[test]
fn filtered_older_block_recalibrates_repeated_visible_pool_without_reusing_old_budget() {
    let (_, expanded) = initial_occurrence_pool();
    let mut initial = expanded.clone();
    initial["input"].as_array_mut().unwrap().truncate(1);
    let mut measured = InputMeasurements::default();
    measured.observe(&initial, &json!({"input_tokens":1000}), "route", None, 99);
    measured.observe(
        &expanded,
        &json!({"input_tokens":10000}),
        "route",
        None,
        100,
    );
    let mut filtered = expanded.clone();
    filtered["input"].as_array_mut().unwrap().remove(1);
    measured.observe(&filtered, &json!({"input_tokens":6000}), "route", None, 101);
    let measured: InputMeasurements =
        serde_json::from_value(serde_json::to_value(measured).unwrap()).unwrap();
    let mut target = filtered.clone();
    target["input"].as_array_mut().unwrap().pop();
    let forecast = measured
        .projected_estimate(&target, &filtered, "route", None)
        .unwrap();
    assert_eq!(
        forecast.tokens,
        neutral_tokens(&measured, &target, &filtered)
    );
    assert!(forecast.tokens > 0 && forecast.tokens < 6000);
    assert_eq!(
        forecast.capacity_tokens(),
        None,
        "an incomplete older block is not measurement authority"
    );
}

#[test]
fn duplicate_opaque_pool_members_remain_ambiguous_even_when_another_identity_changes() {
    let (_, mut source) = initial_occurrence_pool();
    let duplicate = source["input"][1].clone();
    source["input"].as_array_mut().unwrap().insert(2, duplicate);
    let mut measured = InputMeasurements::default();
    measured.observe(&source, &json!({"input_tokens":10000}), "route", None, 100);
    // Remove a distinct opaque identity: changes() alone cannot reject the
    // retained duplicate opaque members, so the pool must still fence them.
    let mut distinct = source.clone();
    distinct["input"].as_array_mut().unwrap().pop();
    assert!(
        measured
            .projected_estimate(&distinct, &source, "route", None)
            .is_none()
    );
    // Changing one duplicate is independently refused by changes().
    let mut duplicate = source.clone();
    duplicate["input"].as_array_mut().unwrap().remove(1);
    assert!(
        measured
            .projected_estimate(&duplicate, &source, "route", None)
            .is_none()
    );
    assert_eq!(
        measured.predict(&source, "route", None).unwrap().tokens,
        10000
    );
}
