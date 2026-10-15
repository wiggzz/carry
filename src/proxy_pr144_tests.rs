//! PR144 private economic regressions. Hosted compilation/RED is mandatory;
//! old-binary loopback tests cannot execute these nonpublic helper calls.
use super::*;

fn config() -> ProxyCli {
    ProxyCli::try_parse_from([
        "carry proxy",
        "--mode",
        "compact",
        "--classifier-model",
        "gpt-6-sol",
        "--classifier-cache-policy",
        "disabled",
        "--background-reviews=false",
        "--payoff-requests",
        "1",
        "--review-every-requests",
        "100",
        "--min-payback-percent",
        "0",
    ])
    .unwrap()
}

fn session_with_review() -> (Session, Value) {
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"retained calibration ".repeat(800)}),
            json!({"role":"assistant","content":"disposable result ".repeat(200)}),
            json!({"role":"user","content":"finish current task"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    let groups = session.groups();
    session.observe_groups(&groups);
    session
        .apply_advice(
            &groups,
            &json!({"protected":["g1"],"removable":["g2"],"memories":[]}),
        )
        .unwrap();
    session.completed_requests = 1;
    session.last_review_request = 0;
    let main = json!({"model":"gpt-6-luna","input":session.render_primary(),"store":false});
    session.review_context = cache_base(&main);
    let mut sources = review_source_input(&session, &groups);
    sources.pop().unwrap();
    let body = review_body(&config(), &session);
    let mut input = body["input"].as_array().unwrap()[1..].to_vec();
    input.push(
        json!({"role":"assistant","content":[{"type":"output_text","text":
        json!({"protected":["g1"],"removable":["g2"],"memories":[]}).to_string()}]}),
    );
    session.review_history = Some(crate::proxy_state::ReviewHistory {
        source_len: sources.len(),
        source_sha256: reviewer_serialized_identity(&sources).1,
        input,
    });
    (session, main)
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "actual {actual} != expected {expected}"
    );
}

/// Price each actual rendered branch independently, including output reserve.
fn scheduled_cost(config: &ProxyCli, original: &Session, branch: &Session, calls: u64) -> f64 {
    if calls == 0 {
        return 0.0;
    }
    let keep = review_body(config, original);
    let compact = review_body(config, branch);
    let input =
        reviewer_view_costs(config, &original.reviewer_cache, &keep, &compact, calls).unwrap();
    let rates = Rates::for_model(&config.classifier_model, estimate(&compact)).unwrap();
    input.compact + calls as f64 * config.classifier_max_output_tokens as f64 * rates.output
}

#[test]
fn planner01_not_due_prices_zero_shadow_and_cannot_repay_primary_cache_loss() {
    let config = config();
    let (mut session, main) = session_with_review();
    observe_cache(
        &mut session.primary_cache,
        &main,
        &json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":100}}),
    );
    let (candidate, report) = plan(&config, &session, &main);
    assert_eq!(
        report["future_reviewer_calls"], 0,
        "age 1/cadence 100/horizon 1 executes no reviews"
    );
    for row in report["candidates"].as_array().unwrap() {
        let joint = &row["joint"];
        close(joint["future_shadow_keep"].as_f64().unwrap(), 0.0);
        close(joint["future_shadow_compact"].as_f64().unwrap(), 0.0);
        assert!(
            joint["primary_keep"].as_f64().unwrap() <= joint["primary_compact"].as_f64().unwrap()
        );
        assert_eq!(
            joint["accepted"], false,
            "nonexistent shadow savings cannot repay a primary loss"
        );
    }
    assert!(candidate.is_none());
}

#[test]
fn planner01_due_source_removal_skips_review_but_reviewer_only_still_dispatches() {
    let mut config = config();
    config.review_every_requests = 1;
    let (session, main) = session_with_review();
    let (_, report) = plan(&config, &session, &main);
    let keep = scheduled_cost(&config, &session, &session, 1);
    assert!(keep > 0.0);
    let mut source_rows = 0;
    let mut reviewer_only_rows = 0;
    for row in report["candidates"].as_array().unwrap() {
        let removed: Vec<u64> = serde_json::from_value(row["removed_groups"].clone()).unwrap();
        let mut branch = session.clone();
        branch.remove(&removed, &session.groups());
        let calls = if removed.is_empty() {
            reviewer_only_rows += 1;
            1
        } else {
            source_rows += 1;
            0
        };
        close(row["joint"]["future_shadow_keep"].as_f64().unwrap(), keep);
        close(
            row["joint"]["future_shadow_compact"].as_f64().unwrap(),
            scheduled_cost(&config, &session, &branch, calls),
        );
        if removed.is_empty() {
            assert_eq!(branch.render_primary(), session.render_primary());
        }
    }
    assert!(
        source_rows > 0 && reviewer_only_rows > 0,
        "both real planner branch types must execute"
    );
}

#[test]
fn planner01_no_eligible_source_means_no_paid_future_review() {
    let mut config = config();
    config.review_every_requests = 1;
    let (mut session, _) = session_with_review();
    // Only the newest pinned user remains; stale reviewer control turns are
    // disposable, not a new source of eligibility.
    session.remove(&[1, 2], &session.groups());
    let main = json!({"model":"gpt-6-luna","input":session.render_primary()});
    let (_, report) = plan(&config, &session, &main);
    assert_eq!(report["future_reviewer_calls"], 0);
}

#[test]
fn planner01_completed_bootstrap_receipts_are_sunk_not_future_calls() {
    let mut config = config();
    config.review_every_requests = 1;
    let (mut session, main) = session_with_review();
    session.bootstrap_review_pending = true;
    session.bootstrap_review_complete = true;
    session.bootstrap_reviewed_ids.extend([1, 2]);
    session.shadow.calls = 2;
    let (_, report) = plan(&config, &session, &main);
    assert_eq!(
        report["future_reviewer_calls"], 0,
        "completed pre-primary bootstrap must not be charged again as an ordinary due review"
    );
    for row in report["candidates"].as_array().unwrap() {
        close(row["joint"]["future_shadow_keep"].as_f64().unwrap(), 0.0);
        close(row["joint"]["future_shadow_compact"].as_f64().unwrap(), 0.0);
    }
    assert_eq!(session.shadow.calls, 2);
}

fn primary(marker: Option<Value>) -> Value {
    let mut body = json!({"model":"gpt-6-sol","input":[{"role":"user","content":[
        {"type":"input_text","text":"actual rendered primary payload ".repeat(200)}]}]});
    if let Some(marker) = marker {
        body["input"][0]["content"][0]["prompt_cache_breakpoint"] = marker;
    }
    body
}

#[test]
fn planner02_identical_explicit_primary_bodies_have_identical_prices() {
    let body = primary(Some(json!({"mode":"explicit"})));
    let cost = view_costs(&body, &body, &[], "gpt-6-sol", 1).unwrap();
    close(cost.keep, cost.compact);
    close(
        cost.keep,
        estimate(&body)
            * Rates::for_model("gpt-6-sol", estimate(&body))
                .unwrap()
                .write,
    );
}

#[test]
fn planner02_null_and_nonexplicit_markers_are_not_cache_writes() {
    for marker in [None, Some(Value::Null), Some(json!({"mode":"implicit"}))] {
        let body = primary(marker);
        let cost = view_costs(&body, &body, &[], "gpt-6-sol", 1).unwrap();
        let expected = estimate(&body)
            * Rates::for_model("gpt-6-sol", estimate(&body))
                .unwrap()
                .input;
        close(cost.keep, expected);
        close(cost.compact, expected);
    }
}

#[test]
fn planner02_identical_body_with_compatible_receipt_still_has_one_price() {
    let body = primary(Some(json!({"mode":"explicit"})));
    let mut receipts = Vec::new();
    observe_cache(
        &mut receipts,
        &body,
        &json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":100}}),
    );
    let cost = view_costs(&body, &body, &receipts, "gpt-6-sol", 1).unwrap();
    close(cost.keep, cost.compact);
    let rates = Rates::for_model("gpt-6-sol", estimate(&body)).unwrap();
    close(
        cost.keep,
        input_cost(
            estimate(&body),
            reusable(&receipts, &body),
            rates.write,
            rates.cached,
        ),
    );
}

#[test]
fn planner02_reviewer_only_candidate_has_zero_primary_price_delta() {
    let mut config = config();
    config.review_every_requests = 1;
    let (mut session, _) = session_with_review();
    session.opinions.insert(2, "keep".into());
    let mut main = primary(Some(json!({"mode":"explicit"})));
    // A private fixture installs the already-exposed marked source; no helper
    // substitute is used for either rendering or pricing the two branches.
    session.history[0].value["content"] = json!([{"type":"input_text","text":"retained calibration ".repeat(800),
        "prompt_cache_breakpoint":{"mode":"explicit"}}]);
    main["input"] = json!(session.render_primary());
    let (_, report) = plan(&config, &session, &main);
    let rows = report["candidates"].as_array().unwrap();
    assert!(
        !rows.is_empty(),
        "disposable review turns must yield a reviewer-only candidate"
    );
    for row in rows {
        assert_eq!(row["removed_groups"], json!([]));
        close(
            row["joint"]["primary_keep"].as_f64().unwrap(),
            row["joint"]["primary_compact"].as_f64().unwrap(),
        );
    }
}
