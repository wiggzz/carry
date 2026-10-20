//! PR144 private economic regressions. Hosted compilation/RED is mandatory;
//! old-binary loopback tests cannot execute these nonpublic helper calls.
use super::*;

fn config() -> ProxyCli {
    ProxyCli::try_parse_from([
        "carry proxy",
        "--review-min-primary-tokens",
        "0",
        "--review-min-new-tokens",
        "0",
        "--review-backoff-factor",
        "1",
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

#[test]
fn planner_final_default_background_prices_future_completion_sensitivity() {
    let mut config = config();
    config.background_reviews = true;
    config.payoff_requests = 5;
    config.review_every_requests = 1;
    let (mut session, main) = session_with_review();
    session.completed_requests = 0;
    session.last_review_request = 0;
    let (_, asynchronous) = plan(&config, &session, &main);
    assert_eq!(
        asynchronous["future_reviewer_calls"], 4,
        "no current pending work: future completion sensitivity must remain priced at default cadence"
    );
    config.background_reviews = false;
    let (_, synchronous) = plan(&config, &session, &main);
    assert_eq!(synchronous["future_reviewer_calls"], 4);
    let async_rows = asynchronous["candidates"].as_array().unwrap();
    let sync_rows = synchronous["candidates"].as_array().unwrap();
    assert!(!async_rows.is_empty());
    assert_eq!(async_rows.len(), sync_rows.len());
    for (asynchronous, synchronous) in async_rows.iter().zip(sync_rows) {
        assert!(synchronous["joint"].is_object());
        assert_eq!(
            asynchronous["removed_groups"],
            synchronous["removed_groups"]
        );
        assert_eq!(asynchronous["joint"], synchronous["joint"]);
    }
}

#[test]
fn planner_final_existing_pending_work_remains_unknown_not_free() {
    let mut config = config();
    config.background_reviews = true;
    config.payoff_requests = 5;
    config.review_every_requests = 1;
    for background in [true, false] {
        let (mut session, main) = session_with_review();
        session.background_review_pending = background;
        session.bootstrap_review_pending = !background;
        session.bootstrap_review_complete = false;
        let (_, report) = plan(&config, &session, &main);
        assert!(report["future_reviewer_calls"].is_null());
        let rows = report["candidates"].as_array().unwrap();
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|row| row["joint"].is_null()));
    }
}

#[test]
fn pending_or_oversized_reviewer_does_not_veto_authorized_primary_savings() {
    for pending in [true, false] {
        let mut config = config();
        config.background_reviews = true;
        config.classifier_max_input_tokens = if pending { 300000 } else { 128 };
        let (mut session, main) = session_with_review();
        session.background_review_pending = pending;
        let (candidate, report) = plan(&config, &session, &main);
        assert_eq!(report["primary_fits"], true);
        assert_eq!(
            report["forced"], false,
            "reviewer capacity never forces primary removal"
        );
        let selected =
            candidate.expect("existing complete-group advice remains independently actionable");
        assert!(
            !selected
                .render_primary()
                .iter()
                .any(|v| v == &session.history[1].value)
        );
        assert!(
            selected
                .render_primary()
                .contains(&session.history[0].value)
        );
        assert!(
            selected
                .render_primary()
                .contains(&session.history[2].value)
        );
        assert_eq!(selected.opinions.get(&1), session.opinions.get(&1));
        if pending {
            assert!(
                report["future_reviewer_calls"].is_null(),
                "pending paid work remains unknown"
            );
        }
    }
}

#[test]
fn oversized_reviewer_without_advice_never_forces_primary_mutation() {
    let mut config = config();
    config.classifier_max_input_tokens = 128;
    let (mut session, main) = session_with_review();
    session.opinions.clear();
    session.drop_members.clear();
    let (candidate, report) = plan(&config, &session, &main);
    assert_eq!(report["reviewer_fits"], false);
    assert_eq!(report["forced"], false);
    if let Some(candidate) = candidate {
        assert_eq!(candidate.render_primary(), session.render_primary());
    }
}

fn warm_reviewer_receipt(config: &ProxyCli, session: &mut Session) -> Value {
    let body = review_body(config, session);
    let count = estimate(&body) as u64;
    // A hit can confirm only a previously transmitted compatible boundary.
    session.reviewer_cache.clear();
    for cached in [0, count] {
        observe_reviewer_cache(
            config,
            &mut session.reviewer_cache,
            &body,
            &json!({"input_tokens":count,"input_tokens_details":{"cached_tokens":cached}}),
        );
    }
    assert!(reviewer_reusable(config, &session.reviewer_cache, &body) > 0.0);
    body
}

#[test]
fn selection_not_due_charges_only_one_conditional_input_rebuild() {
    for pending in [true, false] {
        let mut config = config();
        config.background_reviews = true;
        config.classifier_cache_policy = ClassifierCachePolicy::OpenaiExplicit;
        config.payoff_requests = 5;
        config.review_every_requests = 1;
        let (mut session, main) = session_with_review();
        session.completed_requests = 0;
        session.background_review_pending = pending;
        let mut prior = None;
        for output_cap in [256, 16384] {
            config.classifier_max_output_tokens = output_cap;
            let reviewer = warm_reviewer_receipt(&config, &mut session);
            let mut candidate = session.clone();
            candidate.remove(&[2], &session.groups());
            let compact = review_body(&config, &candidate);
            let cost =
                reviewer_view_costs(&config, &session.reviewer_cache, &reviewer, &compact, 1)
                    .unwrap();
            let allowance = (cost.compact - cost.keep).max(0.0);
            assert!(
                allowance > 0.0,
                "exact warm receipt must expose a real invalidation loss"
            );
            let (_, report) = plan(&config, &session, &main);
            let row = report["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["removed_groups"] == json!([2]))
                .unwrap();
            close(
                row["selection_joint"]["future_shadow_keep"]
                    .as_f64()
                    .unwrap(),
                0.0,
            );
            close(
                row["selection_joint"]["future_shadow_compact"]
                    .as_f64()
                    .unwrap(),
                allowance,
            );
            assert!(row["selection_reviewer_output_completions"].is_null());
            if let Some(prior) = &prior {
                assert_eq!(prior, &row["selection_joint"]);
            }
            prior = Some(row["selection_joint"].clone());
            if pending {
                assert!(
                    row["joint"].is_null(),
                    "unfinished recurring total remains unknown"
                );
            } else {
                assert_eq!(
                    report["future_reviewer_calls"], 4,
                    "the protected timely-completion forecast is a distinct diagnostic"
                );
            }
        }
    }
}

#[test]
fn selection_due_source_rewrite_charges_real_next_input_invalidation() {
    let mut config = config();
    config.background_reviews = true;
    config.classifier_cache_policy = ClassifierCachePolicy::OpenaiExplicit;
    config.review_every_requests = 1;
    let (mut session, main) = session_with_review();
    let reviewer = warm_reviewer_receipt(&config, &mut session);
    let mut candidate = session.clone();
    candidate.remove(&[2], &session.groups());
    let compact = review_body(&config, &candidate);
    let cost =
        reviewer_view_costs(&config, &session.reviewer_cache, &reviewer, &compact, 1).unwrap();
    let allowance = (cost.compact - cost.keep).max(0.0);
    assert!(allowance > 0.0);
    let (_, report) = plan(&config, &session, &main);
    let row = report["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["removed_groups"] == json!([2]))
        .unwrap();
    assert_eq!(row["selection_review_basis"], "presently_due_next_review");
    assert_eq!(row["selection_reviewer_output_completions"], 0);
    close(
        row["selection_joint"]["future_shadow_compact"]
            .as_f64()
            .unwrap(),
        allowance,
    );
    assert!(
        row["selection_joint"]["future_shadow_keep"]
            .as_f64()
            .unwrap()
            > cost.keep,
        "presently due keep branch includes one output cap, not a free completion"
    );
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
