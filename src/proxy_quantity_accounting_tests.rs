use super::*;
use serde_json::json;

fn reasoning(id: &str) -> Value {
    json!({"type":"reasoning", "id":id, "summary":[], "encrypted_content":format!("opaque-{id}")})
}
fn request(values: Vec<Value>) -> Value {
    json!({"model":"gpt-6.1-sol", "input":values})
}
fn observe(m: &mut InputMeasurements, body: &Value, input: u64, at: u64) {
    m.observe(body, &json!({"input_tokens":input}), "route", None, at);
}

#[test]
fn neutral_accounting_joint_residual_conserves_whole_and_partial_blocks() {
    let mut m = InputMeasurements::default();
    let base = request(vec![json!({"role":"user", "content":"base"})]);
    observe(&mut m, &base, 1000, 1);
    let r1 = reasoning("one");
    let r2 = reasoning("two");
    let visible = json!({"role":"user", "content":"a visible addition"});
    let added = vec![r1.clone(), r2.clone(), visible.clone()];
    let local: u64 = added.iter().map(crate::savings::local_text_tokens).sum();
    let mut canonical = base.clone();
    canonical["input"].as_array_mut().unwrap().extend(added);
    observe(&mut m, &canonical, 1000 + local + 2001, 2);
    let one = request(vec![base["input"][0].clone(), r2.clone(), visible.clone()]);
    let two = request(vec![base["input"][0].clone(), r1.clone(), visible.clone()]);
    assert!(
        m.restoration(&canonical, &one, "route", None).is_none(),
        "strict measurement API stays strict"
    );
    let q1 = m
        .accounting_restoration(&canonical, &one, "route", None)
        .unwrap();
    let q2 = m
        .accounting_restoration(&canonical, &two, "route", None)
        .unwrap();
    assert_eq!(q1.estimated_opaque_items, 1);
    assert_eq!(q2.estimated_opaque_items, 1);
    let whole = m
        .accounting_restoration(&canonical, &base, "route", None)
        .unwrap();
    assert_eq!(whole.estimated_opaque_items, 0);
    assert!(whole.measured);
    assert!(!q1.measured);
    assert_eq!(whole.tokens, (local + 2001) as i128);
    assert_eq!(
        q1.tokens + q2.tokens + crate::savings::local_text_tokens(&visible) as i128,
        whole.tokens
    );
    assert_eq!(
        m.accounting_restoration(&one, &canonical, "route", None)
            .unwrap()
            .tokens,
        -q1.tokens
    );
    let mut m: InputMeasurements =
        serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
    assert_eq!(
        m.accounting_restoration(&canonical, &one, "route", None)
            .unwrap()
            .tokens,
        q1.tokens
    );
    let persisted = serde_json::to_string(&m).unwrap();
    assert!(!persisted.contains("opaque-one"));
}

#[test]
fn neutral_accounting_initial_opaque_needs_compatible_increment_not_initial_residual() {
    let initial = request(vec![
        reasoning("initial"),
        json!({"role":"user", "content":"base"}),
    ]);
    let submitted = request(vec![initial["input"][1].clone()]);
    let mut m = InputMeasurements::default();
    observe(&mut m, &initial, 50000, 1);
    assert!(
        m.accounting_restoration(&initial, &submitted, "route", None)
            .is_none()
    );
    let mut appended = initial.clone();
    let new = reasoning("calibration");
    let local = crate::savings::local_text_tokens(&new);
    appended["input"].as_array_mut().unwrap().push(new);
    observe(&mut m, &appended, 50000 + local + 2000, 2);
    let q = m
        .accounting_restoration(&initial, &submitted, "route", None)
        .unwrap();
    assert_eq!(
        q.tokens,
        (crate::savings::local_text_tokens(&initial["input"][0]) + 2000) as i128
    );
    assert_eq!(q.estimated_opaque_items, 1);
    assert!(
        m.accounting_restoration(&initial, &submitted, "other-route", None)
            .is_none()
    );
    let mut changed = initial.clone();
    changed["instructions"] = json!("different");
    assert!(
        m.accounting_restoration(&changed, &submitted, "route", None)
            .is_none()
    );
    observe(&mut m, &appended, 50000 + local + 3000, 3);
    assert!(
        m.accounting_restoration(&initial, &submitted, "route", None)
            .is_none(),
        "receipt drift invalidates calibration"
    );
}

#[test]
fn neutral_accounting_request_time_held_out_receipts() {
    let mut m = InputMeasurements::default();
    let mut body = request(vec![json!({"role":"user", "content":"base"})]);
    let mut input = 1000u64;
    observe(&mut m, &body, input, 1);
    for (i, residual) in [1000, 2000, 3000].into_iter().enumerate() {
        let item = reasoning(&format!("train-{i}"));
        input += crate::savings::local_text_tokens(&item) + residual;
        body["input"].as_array_mut().unwrap().push(item);
        observe(&mut m, &body, input, i as u64 + 2);
    }
    // Predictions are frozen BEFORE seeing any held-out native receipt. These
    // synthetic controls test units/leakage and expose errors, not real-world
    // statistical calibration or proof of no-Carry causal expenditure.
    let mut results = Vec::new();
    for (i, actual_residual) in [1800u64, 2000, 2300].into_iter().enumerate() {
        let item = reasoning(&format!("held-out-{i}"));
        let mut canonical = body.clone();
        canonical["input"]
            .as_array_mut()
            .unwrap()
            .push(item.clone());
        let prediction = m
            .accounting_restoration(&canonical, &body, "route", None)
            .unwrap();
        let actual = crate::savings::local_text_tokens(&item) + actual_residual;
        results.push(json!({"prediction":prediction.tokens,"native_increment":actual,"error":prediction.tokens - actual as i128}));
        assert_eq!(
            prediction.tokens,
            (crate::savings::local_text_tokens(&item) + 2000) as i128
        );
    }
    println!("HELD_OUT_RECEIPTS={}", json!(results));
    assert_eq!(
        m.blocks.len(),
        3,
        "held-out receipts cannot train their own predictions"
    );
    let output = json!([reasoning("future-output")]);
    assert_eq!(m.retained_reasoning_extra(&output), Some((2000, 1)));
    assert_eq!(
        InputMeasurements::default().retained_reasoning_extra(&output),
        None
    );
}

#[test]
fn rejected_native_block_never_borrows_an_older_residual() {
    let mut m = InputMeasurements::default();
    let base = request(vec![json!({"role":"user", "content":"base"})]);
    observe(&mut m, &base, 1000, 1);
    let train = reasoning("training");
    let trained = request(vec![base["input"][0].clone(), train.clone()]);
    let input = 2000 + crate::savings::local_text_tokens(&train);
    observe(&mut m, &trained, input, 2);
    let block = vec![
        reasoning("bad-one"),
        reasoning("bad-two"),
        json!({"role":"user", "content":"visible floor"}),
    ];
    let mut canonical = trained.clone();
    canonical["input"].as_array_mut().unwrap().extend(block);
    observe(&mut m, &canonical, input + 1, 3);
    let mut legacy = serde_json::to_value(&m).unwrap();
    for block in legacy["blocks"].as_array_mut().unwrap() {
        block.as_object_mut().unwrap().remove("allocation");
        block.as_object_mut().unwrap().remove("allocation_rejected");
        block.as_object_mut().unwrap().remove("opaque");
    }
    for mut m in [
        m.clone(),
        serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap(),
        serde_json::from_value(legacy).unwrap(),
    ] {
        for index in [2, 3] {
            let mut partial = canonical.clone();
            partial["input"].as_array_mut().unwrap().remove(index);
            assert!(
                m.accounting_restoration(&canonical, &partial, "route", None)
                    .is_none(),
                "partial native budget 1 must not borrow pooled residual"
            );
        }
        assert_eq!(
            m.accounting_restoration(&canonical, &trained, "route", None)
                .unwrap()
                .tokens,
            1,
            "whole native block remains authoritative"
        );
        let mut restarted: InputMeasurements =
            serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        let mut partial = canonical.clone();
        partial["input"].as_array_mut().unwrap().remove(2);
        assert!(
            restarted
                .accounting_restoration(&canonical, &partial, "route", None)
                .is_none()
        );
    }
}

#[test]
fn visible_only_native_block_keeps_partial_local_text_attribution() {
    let mut m = InputMeasurements::default();
    let base = request(vec![json!({"role":"user","content":"base"})]);
    observe(&mut m, &base, 1000, 1);
    let one = json!({"role":"user","content":"first visible"});
    let two = json!({"role":"user","content":"second visible"});
    let appended = request(vec![base["input"][0].clone(), one.clone(), two.clone()]);
    observe(&mut m, &appended, 2000, 2);
    let partial = request(vec![base["input"][0].clone(), two]);
    assert_eq!(
        m.accounting_restoration(&appended, &partial, "route", None)
            .unwrap()
            .tokens,
        crate::savings::local_text_tokens(&one) as i128
    );
}

#[test]
fn rejected_partial_quantity_never_mutates_settled_native_money() {
    let mut m = InputMeasurements::default();
    let base = request(vec![json!({"role":"user","content":"base"})]);
    observe(&mut m, &base, 1000, 1);
    let trained = request(vec![base["input"][0].clone(), reasoning("train-money")]);
    observe(&mut m, &trained, 3000, 2);
    let mut canonical = trained.clone();
    canonical["input"].as_array_mut().unwrap().extend([
        reasoning("bad-money-one"),
        reasoning("bad-money-two"),
        json!({"role":"user","content":"floor"}),
    ]);
    observe(&mut m, &canonical, 3001, 3);
    let mut partial = canonical.clone();
    partial["input"].as_array_mut().unwrap().remove(2);
    let q = m.accounting_restoration(&canonical, &partial, "route", None);
    assert!(q.is_none());
    let mut t = crate::proxy_savings::Trajectory::default();
    t.projected_evidence(q, true);
    t.primary_settled(crate::proxy_savings::PrimaryObservation {body:&partial,response:&json!({"status":"completed","output":[],"usage":{"input_tokens":2900,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}}),route:"route",at:4,standard:true,resolved:None,cost:Some(0.125),measurements:Some(&m)});
    assert_eq!(t.snapshot().observed_primary_cost, 0.125);
    assert_eq!(t.snapshot().matched_primary_responses, 0);
    let saved = serde_json::to_vec(&t).unwrap();
    let restarted: crate::proxy_savings::Trajectory = serde_json::from_slice(&saved).unwrap();
    assert_eq!(restarted.snapshot().observed_primary_cost, 0.125);
}

#[test]
fn neutral_accounting_never_uses_ciphertext_size_or_unknown_modality() {
    let m = InputMeasurements::default();
    let one = reasoning("one");
    let mut huge = one.clone();
    huge["encrypted_content"] = json!("x".repeat(100_000));
    assert_eq!(
        m.residual_allocation(&[one], 4000).unwrap().tokens,
        m.residual_allocation(&[huge], 4000).unwrap().tokens
    );
    let image = json!({"role":"user", "content":[{"type":"input_image", "image_url":"opaque"}]});
    assert!(m.residual_allocation(&[image], 4000).is_none());
    assert!(
        m.residual_allocation(&[reasoning("impossible")], 0)
            .is_none()
    );
}
