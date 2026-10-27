// Ordinary policy controls independent of deleted import/replay/lease seams.
fn admission_subject() -> Session {
    let mut session = Session::default();
    session
        .ingest(&[
            json!({"role":"user","content":"original goal"}),
            json!({"role":"assistant","content":"native evidence ".repeat(100)}),
            json!({"role":"user","content":"current goal"}),
        ])
        .unwrap();
    for item in &mut session.history {
        item.exposed = true;
    }
    session.completed_requests = 4;
    session
}

fn admission_main(session: &Session) -> Value {
    json!({"model":"gpt-6-luna","input":session.render_primary()})
}

#[test]
fn ordinary_admission_floor_age_and_unique_native_growth_are_conjunctive() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    let mut session = admission_subject();
    let main = admission_main(&session);
    let primary = admission::primary_tokens(&config, &session, &main) as u32;
    let growth =
        admission::decision(&config, &session, &main, false)["new_native_source_estimated_tokens"]
            .as_f64()
            .unwrap() as u32;
    assert!(growth > 0);
    config.review_min_primary_tokens = primary + 1;
    config.review_min_new_tokens = growth;
    assert_eq!(
        admission::decision(&config, &session, &main, false)["reason"],
        "primary_floor"
    );
    config.review_min_primary_tokens = primary;
    session.completed_requests = 3;
    assert_eq!(
        admission::decision(&config, &session, &main, false)["reason"],
        "cadence"
    );
    session.completed_requests = 400;
    config.review_min_new_tokens = growth + 1;
    assert_eq!(
        admission::decision(&config, &session, &main, false)["reason"],
        "native_source_growth"
    );
    config.review_min_new_tokens = growth;
    assert_eq!(
        admission::decision(&config, &session, &main, false)["reason"],
        "admitted"
    );
    let groups = session.groups();
    admission::cover(&mut session, &groups);
    for resumed in [
        session.clone(),
        serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap(),
    ] {
        let decision = admission::decision(&config, &resumed, &main, false);
        assert_eq!(decision["new_native_source_estimated_tokens"], 0.0);
        assert_eq!(decision["reason"], "native_source_growth");
    }
}

#[test]
fn ordinary_backoff_classifies_new_protection_repeats_and_useful_removal() {
    let config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    let mut session = admission_subject();
    for expected in [2, 4, 8, 8] {
        let before = session.clone();
        let groups = session.groups();
        assert_eq!(
            admission::complete(&before, &mut session, true, &groups),
            "no_yield"
        );
        assert_eq!(admission::multiplier(&config, &session), expected);
    }
    let before = session.clone();
    let groups = session.groups();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[2],"removable":[],"memories":[]}),
        )
        .unwrap();
    assert_eq!(
        admission::complete(&before, &mut session, true, &groups),
        "protection_progress"
    );
    assert_eq!(admission::multiplier(&config, &session), 8);
    let before = session.clone();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[2],"memories":[]}),
        )
        .unwrap();
    assert_eq!(
        admission::complete(&before, &mut session, true, &groups),
        "productive"
    );
    assert_eq!(admission::multiplier(&config, &session), 1);
    let before = session.clone();
    session
        .apply_advice(
            &groups,
            &json!({"protected":[],"removable":[2],"memories":[]}),
        )
        .unwrap();
    assert_eq!(
        admission::complete(&before, &mut session, true, &groups),
        "no_yield"
    );
    assert_eq!(admission::multiplier(&config, &session), 2);
    let restored: Session =
        serde_json::from_value(serde_json::to_value(&session).unwrap()).unwrap();
    assert_eq!(admission::multiplier(&config, &restored), 2);
}

#[test]
fn invalid_review_does_not_cover_source_or_increase_backoff_and_unexposed_source_cannot_admit() {
    let mut config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    config.review_min_primary_tokens = 0;
    config.review_min_new_tokens = 1;
    let before = admission_subject();
    let mut after = before.clone();
    let groups = after.groups();
    assert_eq!(
        admission::complete(&before, &mut after, false, &groups),
        "failed_or_invalid"
    );
    let coverage = after.review_admission.as_ref().unwrap();
    assert_eq!(coverage.no_yield_streak, 0);
    assert!(coverage.source_ids.is_empty());
    assert_eq!(
        admission::decision(&config, &after, &admission_main(&after), false)["reason"],
        "admitted"
    );
    for item in &mut after.history {
        item.exposed = false;
    }
    let decision = admission::decision(&config, &after, &admission_main(&after), false);
    assert_eq!(decision["new_native_source_estimated_tokens"], 0.0);
    assert_eq!(decision["reason"], "no_eligible_sources");
}

#[test]
fn ordinary_policy_defaults_and_explicit_bounds_are_not_legacy_aliases() {
    let config = ProxyCli::try_parse_from(["carry proxy"]).unwrap();
    assert_eq!(config.review_min_primary_tokens, 150_000);
    assert_eq!(config.review_min_new_tokens, 25_000);
    assert_eq!(config.review_every_requests, 4);
    assert_eq!(config.review_backoff_factor, 2);
    assert_eq!(config.review_backoff_max, 8);
    for (flag, value) in [
        ("--review-every-requests", "0"),
        ("--review-min-primary-tokens", "2000001"),
        ("--review-min-new-tokens", "2000001"),
        ("--review-backoff-factor", "17"),
        ("--review-backoff-max", "65"),
    ] {
        assert!(ProxyCli::try_parse_from(["carry proxy", flag, value]).is_err());
    }
    let config = ProxyCli::try_parse_from([
        "carry proxy",
        "--review-min-primary-tokens",
        "0",
        "--review-min-new-tokens",
        "0",
        "--review-every-requests",
        "1",
        "--review-backoff-factor",
        "1",
    ])
    .unwrap();
    assert_eq!(config.review_min_primary_tokens, 0);
    assert_eq!(config.review_min_new_tokens, 0);
    assert_eq!(config.review_every_requests, 1);
    assert_eq!(admission::multiplier(&config, &admission_subject()), 1);
}
