//! Issue148 standalone reviewer-only acceptance. Synthetic native replay/receipts;
//! no provider traffic. Existing behavior controls, not a reproduced defect.
use super::*;

fn config() -> ProxyCli {
    let mut config = ProxyCli::try_parse_from([
        "carry proxy",
        "--mode",
        "compact",
        "--classifier-model",
        "gpt-6-sol",
        "--classifier-cache-policy",
        "disabled",
        "--review-min-primary-tokens",
        "0",
        "--review-min-new-tokens",
        "0",
        "--review-backoff-factor",
        "1",
        "--review-every-requests",
        "1",
        "--payoff-requests",
        "1",
        "--min-payback-percent",
        "0",
        "--background-reviews=false",
    ])
    .unwrap();
    config.context_margin_percent = 0;
    config.primary_context_tokens = 1_000_000;
    config.classifier_max_input_tokens = 1_000_000;
    config
}

fn replay(config: &ProxyCli, retained: usize, disposable: usize) -> (Session, Value) {
    let mut session = Session::default();
    session.initialize_keep_leases(config.keep_lease_turns);
    session
        .ingest(&[
            json!({"role":"user","content":"durable ".repeat(retained)}),
            json!({"role":"assistant","content":"still relevant outcome"}),
            json!({"role":"user","content":"continue"}),
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
            &json!({
                "protected":["g1","g2"], "removable":[],
                "memories":[{"source_ids":["g2"],"text":"retained accepted fact"}]
            }),
        )
        .unwrap();
    session.completed_requests = 1;
    let main = json!({"model":"gpt-6-luna","input":session.render_primary(),"store":false});
    let primary_tokens = estimate(&main) as u64;
    observe_cache(
        &mut session.primary_cache,
        &main,
        &json!({"input_tokens":primary_tokens,"input_tokens_details":{"cached_tokens":primary_tokens}}),
    );
    assert!(!session.primary_cache.is_empty());
    session.review_context = cache_base(&main);
    session.reviewer_controls = Some(review_control_state(&session));
    let mut sources = review_source_input(&session, &groups);
    sources.pop().unwrap();
    let source_len = sources.len();
    let source_sha256 = reviewer_serialized_identity(&sources).1;
    sources.push(json!({"role":"assistant","content":[{"type":"output_text","text":"disposable review turn ".repeat(disposable)}]}));
    session.review_history = Some(crate::proxy_state::ReviewHistory {
        source_len,
        source_sha256,
        input: sources,
    });
    assert!(estimate(&review_body(config, &session)) > 0.0);
    (session, main)
}

fn only_row(report: &Value) -> &Value {
    let rows = report["candidates"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "protected sources leave only reviewer cleanup: {report}"
    );
    assert_eq!(rows[0]["action"], "reviewer_only");
    assert_eq!(rows[0]["removed_groups"], json!([]));
    &rows[0]
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
}

fn preserve(before: &Session, after: &Session) {
    assert_eq!(after.render_primary(), before.render_primary());
    assert_eq!(after.opinions, before.opinions);
    assert_eq!(after.drop_members, before.drop_members);
    assert_eq!(
        serde_json::to_value(&after.memories).unwrap(),
        serde_json::to_value(&before.memories).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&after.primary_cache).unwrap(),
        serde_json::to_value(&before.primary_cache).unwrap()
    );
    assert_eq!(after.compactions, before.compactions);
}

#[test]
fn reviewer_only148_tier_crossing_prices_each_rendering_and_output() {
    let config = config();
    let (session, main) = replay(&config, 800, 55_000);
    let keep = review_body(&config, &session);
    let mut clean = session.clone();
    clean.compact_reviewer();
    let rebuilt = review_body(&config, &clean);
    let kt = estimate(&keep);
    let ct = estimate(&rebuilt);
    assert!(
        kt > 272_000.0 && ct < 272_000.0,
        "actual reviewer tiers {kt}/{ct}"
    );
    let kr = Rates::for_model(&config.classifier_model, kt).unwrap();
    let cr = Rates::for_model(&config.classifier_model, ct).unwrap();
    assert!(kr.input > cr.input && kr.output > cr.output);
    let (selected, report) = plan(&config, &session, &main);
    let row = only_row(&report);
    let joint = &row["selection_joint"];
    let output = f64::from(config.classifier_max_output_tokens);
    // Independent scalar oracle: no cache receipts and one presently due call.
    close(
        joint["future_shadow_keep"].as_f64().unwrap(),
        kt * kr.input + output * kr.output,
    );
    close(
        joint["future_shadow_compact"].as_f64().unwrap(),
        ct * cr.input + output * cr.output,
    );
    close(
        joint["primary_keep"].as_f64().unwrap(),
        joint["primary_compact"].as_f64().unwrap(),
    );
    assert_eq!(report["selected_action"], "reviewer_only");
    let selected = selected.unwrap();
    preserve(&session, &selected);
    assert!(selected.review_history.is_none());
}

#[test]
fn reviewer_only148_compatible_warm_receipts_reverse_cold_rebuild_selection() {
    let config = config();
    let (mut session, main) = replay(&config, 20_000, 1_000);
    let (cold, cold_report) = plan(&config, &session, &main);
    assert!(
        cold.is_some(),
        "cold disposable replay repays its rebuild: {cold_report}"
    );
    let keep = review_body(&config, &session);
    let tokens = estimate(&keep) as u64;
    for cached in [0, tokens] {
        observe_reviewer_cache(
            &config,
            &mut session.reviewer_cache,
            &keep,
            &json!({"input_tokens":tokens,"input_tokens_details":{"cached_tokens":cached}}),
        );
    }
    assert!(reviewer_reusable(&config, &session.reviewer_cache, &keep) > 0.0);
    let (warm, warm_report) = plan(&config, &session, &main);
    assert!(
        warm.is_none(),
        "warm primary-independent reviewer prefix must not be thrown away: {warm_report}"
    );
    assert!(
        only_row(&warm_report)["selection_joint"]["future_shadow_compact"]
            .as_f64()
            .unwrap()
            > only_row(&warm_report)["selection_joint"]["future_shadow_keep"]
                .as_f64()
                .unwrap()
    );
    // Receipt compatibility is not a generic warm flag; expiry makes it cold.
    for receipt in &mut session.reviewer_cache {
        receipt.at = now().saturating_sub(1801);
    }
    assert!(plan(&config, &session, &main).0.is_some());
}

#[test]
fn reviewer_only148_output_allowance_moves_strict_break_even() {
    let mut config = config();
    let (session, main) = replay(&config, 800, 4_000);
    let mut ratios = Vec::new();
    for output in [1, 16_384] {
        config.classifier_max_output_tokens = output;
        let (_, report) = plan(&config, &session, &main);
        let row = only_row(&report);
        let joint = &row["selection_joint"];
        assert!(joint["savings"].as_f64().unwrap() > 0.0);
        ratios.push(100.0 * joint["savings"].as_f64().unwrap() / joint["keep"].as_f64().unwrap());
    }
    let margin = ((ratios[0] + ratios[1]) / 2.0).floor() as u8;
    assert!(ratios[1] < f64::from(margin) && f64::from(margin) < ratios[0]);
    config.min_payback_percent = margin;
    for (output, accepted) in [(1, true), (16_384, false)] {
        config.classifier_max_output_tokens = output;
        let (selected, report) = plan(&config, &session, &main);
        assert_eq!(selected.is_some(), accepted, "{report}");
        assert_eq!(only_row(&report)["selection_joint"]["accepted"], accepted);
    }
}

#[test]
fn reviewer_only148_forced_capacity_cleanup_preserves_primary_authority() {
    let mut config = config();
    config.min_payback_percent = 100;
    let (mut session, main) = replay(&config, 800, 4_000);
    let mut clean = session.clone();
    clean.compact_reviewer();
    config.classifier_max_input_tokens =
        reviewer_required_tokens(&config, &review_body(&config, &clean)).ceil() as u32;
    assert!(!review_fits(&config, &review_body(&config, &session)));
    assert!(review_fits(&config, &review_body(&config, &clean)));
    let (_, report) = plan(&config, &session, &main);
    assert_eq!(report["primary_fits"], true);
    assert_eq!(
        report["forced"], false,
        "reviewer pressure never forces primary removal"
    );
    let before = session.clone();
    super::admission::initialize(&mut session);
    let groups = session.groups();
    let body = prepare_ordinary_review(&config, &mut session, &groups, "synthetic-capacity148");
    assert!(review_fits(&config, &body));
    assert!(session.review_history.is_none());
    assert_eq!(
        session.reviewer_compactions,
        before.reviewer_compactions + 1
    );
    preserve(&before, &session);
}

#[test]
fn reviewer_only148_no_due_or_pending_review_does_not_invent_savings() {
    let mut config = config();
    config.review_every_requests = 100;
    config.payoff_requests = 5;
    let (session, main) = replay(&config, 800, 4_000);
    let (selected, report) = plan(&config, &session, &main);
    assert!(selected.is_none());
    assert_eq!(report["future_reviewer_calls"], 0);
    assert_eq!(
        only_row(&report)["selection_review_basis"],
        "conditional_one_request_input_rebuild_allowance_no_review_savings"
    );
    config.review_every_requests = 1;
    let mut pending = session.clone();
    pending.background_review_pending = true;
    let (selected, report) = plan(&config, &pending, &main);
    assert!(selected.is_none());
    assert!(report["future_reviewer_calls"].is_null());
    assert!(only_row(&report)["joint"].is_null());
    assert!(only_row(&report)["selection_reviewer_output_completions"].is_null());
    // Past paid reviewer usage is not charged again as a rebuild/acquisition fee.
    let (selected, original) = plan(&config, &session, &main);
    let mut paid = session.clone();
    paid.shadow.observe("gpt-6-sol", &json!({"input_tokens":100000,"output_tokens":20000,"input_tokens_details":{"cached_tokens":0}}), true);
    let (paid_selected, paid_report) = plan(&config, &paid, &main);
    assert_eq!(paid_selected.is_some(), selected.is_some());
    assert_eq!(
        only_row(&paid_report)["selection_joint"],
        only_row(&original)["selection_joint"]
    );
}
