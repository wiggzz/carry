// Acceptance for the shared modeled transition after the two real-coordinator
// tests-first failures. These controls are not additional reproduced defects.
#[test]
fn modeled_read_coverage_keeps_original_writes_and_partial_append_extents() {
    let mut cache = ModeledCache::default();
    cache.requested("scope".into(), 100, 1800, 2000, Some(1800));
    assert!(cache.reads.is_empty(), "a cold write is not a read");
    let original = serde_json::to_value(&cache.writes).unwrap();
    cache.requested("scope".into(), 1899, 1800, 2100, Some(1800));
    assert_eq!(cache.reads.len(), 1);
    assert_eq!(cache.reads[0].end, 2000, "new suffix was not read");
    assert_eq!(cache.writes.len(), 2);
    assert_eq!(serde_json::to_value(&cache.writes[0]).unwrap(), original[0]);
    assert_eq!(cache.writes[1].start, 2000);
    assert_eq!(cache.writes[1].at, 1899);
    let frozen = serde_json::to_vec(&cache).unwrap();
    assert_eq!(eligible(&cache, "scope", 1901, 2100), 2100);
    assert_eq!(eligible(&cache, "scope", 3699, 2100), 0);
    assert_eq!(eligible(&cache, "other-scope", 1901, 2100), 0);
    assert_eq!(
        serde_json::to_vec(&cache).unwrap(),
        frozen,
        "queries are pure"
    );
    let resumed: ModeledCache = serde_json::from_slice(&frozen).unwrap();
    assert_eq!(eligible(&resumed, "scope", 1901, 2100), 2100);
    assert_eq!(eligible(&resumed, "scope", 3699, 2100), 0);
}

#[test]
fn modeled_shorter_read_cannot_erase_larger_or_longer_earlier_coverage() {
    let mut cache = ModeledCache::default();
    cache.written("scope".into(), 100, 1800, 3000);
    cache.requested("scope".into(), 1800, 1800, 3000, Some(1800));
    cache.requested("scope".into(), 1899, 1800, 2000, Some(1800));
    assert_eq!(eligible(&cache, "scope", 2000, 3000), 3000);
    assert_eq!(eligible(&cache, "scope", 3600, 3000), 2000);
    assert_eq!(eligible(&cache, "scope", 3699, 3000), 0);
    assert_eq!(cache.writes[0].at, 100);
    // Artificial longer original point-write: do not claim its tariff/provider
    // support. This checks the container never shortens independent coverage.
    let mut longer = ModeledCache::default();
    longer.written("scope".into(), 100, 4000, 3000);
    longer.requested("scope".into(), 1800, 1800, 3000, Some(1800));
    assert_eq!(eligible(&longer, "scope", 3600, 3000), 3000);
    assert_eq!(longer.writes[0].at, 100);
    assert_eq!(longer.writes[0].lifetime, 4000);
}

#[test]
fn modeled_no_contract_and_unknown_suffix_cannot_be_refreshed() {
    for refresh in [None, Some(0)] {
        let mut cache = ModeledCache::default();
        cache.requested("scope".into(), 100, 7, 2000, refresh);
        cache.requested("scope".into(), 106, 7, 2000, refresh);
        assert!(cache.reads.is_empty());
        assert_eq!(eligible(&cache, "scope", 107, 2000), 0);
        assert_eq!(cache.writes[0].at, 100);
    }
    let mut uncertain = ModeledCache::default();
    uncertain.written("scope".into(), 100, 1800, 2000);
    uncertain.retain_uncertainty(2000, 3000, 100, 1800);
    uncertain.requested("scope".into(), 101, 1800, 3000, Some(1800));
    assert!(uncertain.reads.is_empty());
    assert_eq!(uncertain.eligible_at("scope", 102, 3000), None);
    assert_eq!(uncertain.writes.len(), 1);
}

#[test]
fn modeled_read_pressure_is_bounded_numeric_and_never_rewrites_original_money_inputs() {
    let mut cache = ModeledCache::default();
    cache.written("scope".into(), 100, 1800, 4000);
    let writes = serde_json::to_value(&cache.writes).unwrap();
    // Decreasing covered sizes retain independently later deadlines. Once the
    // bound is reached, discarded coverage is unknown, not a fabricated miss.
    for step in 0..132 {
        cache.requested("scope".into(), 101 + step, 1800, 4000 - step, Some(1800));
    }
    assert!(cache.reads.len() <= 128);
    assert_eq!(serde_json::to_value(&cache.writes).unwrap(), writes);
    let bytes = serde_json::to_vec(&cache).unwrap();
    assert!(bytes.len() < 16000);
    assert!(!String::from_utf8(bytes.clone()).unwrap().contains("prompt"));
    let resumed: ModeledCache = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(resumed.eligible_at("scope", 2029, 4000), None);
}

#[test]
fn modeled_read_policy_is_frozen_and_separate_from_native_entitlement() {
    use carry::models::ScenarioCachePolicy;
    let policy = ScenarioCachePolicy::for_model("gpt-6.1-sol");
    let body = json!({"model":"gpt-6.1-sol","input":[]});
    assert_eq!(
        policy.read_refresh_seconds("gpt-6.1-sol", &body),
        Some(1800)
    );
    for options in [json!({}), json!({"mode":"implicit","ttl":"30m"})] {
        let mut other = body.clone();
        other["prompt_cache_options"] = options;
        assert_eq!(
            policy.read_refresh_seconds("gpt-6.1-sol", &other),
            Some(1800)
        );
    }
    for options in [
        json!(null),
        json!({"mode":"explicit"}),
        json!({"ttl":"12h"}),
        json!({"ttl":"2h"}),
        json!({"mode":"unknown"}),
    ] {
        let mut other = body.clone();
        other["prompt_cache_options"] = options;
        assert_eq!(policy.read_refresh_seconds("gpt-6.1-sol", &other), None);
    }
    for field in ["version", "ttl_seconds", "minimum_prefix_tokens"] {
        let mut value = serde_json::to_value(&policy).unwrap();
        value[field] = if field == "minimum_prefix_tokens" {
            Value::Null
        } else {
            json!(7)
        };
        let other: ScenarioCachePolicy = serde_json::from_value(value).unwrap();
        assert_eq!(other.read_refresh_seconds("gpt-6.1-sol", &body), None);
    }
    assert_eq!(policy.read_refresh_seconds("unknown-model", &body), None);
    let mut explicit = body.clone();
    explicit["input"] = json!([{"role":"user","content":"prefix",
        "prompt_cache_breakpoint":{"mode":"explicit"}}]);
    assert_eq!(policy.read_refresh_seconds("gpt-6.1-sol", &explicit), None);
    explicit = body;
    explicit["prompt_cache_retention"] = json!("24h");
    assert_eq!(policy.read_refresh_seconds("gpt-6.1-sol", &explicit), None);
}
