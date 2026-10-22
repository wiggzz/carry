//! All metadata here is synthetic, not vendor entitlement or tariff evidence.
use carry::models::{Route, SourceCatalog, ordinary_policy, resolve_model};
use serde_json::{Value, json};

fn source(route: &Route, facts: Value) -> SourceCatalog {
    let parsed = SourceCatalog::from_codex(&json!({"models":[facts]}), 123, true).unwrap();
    let mut projection = serde_json::to_value(parsed).unwrap();
    projection["route"] = serde_json::to_value(route).unwrap();
    serde_json::from_value(projection).unwrap()
}
fn facts() -> Value {
    json!({"slug":"gpt-6-sol","visibility":"list","context_window":400000,
        "max_context_window":800000,"auto_compact_token_limit":360000,
        "effective_context_window_percent":95,"comp_hash":"synthetic-v1"})
}

#[test]
fn source_facts_are_qualified_by_exact_destination_not_only_subscription_class() {
    let route_a = Route::new("http://127.0.0.1:11001/v1/responses", true);
    let route_b = Route::new("http://127.0.0.1:11002/v1/responses", true);
    let source = source(&route_a, facts());
    let same = resolve_model("gpt-6-sol", route_a, Some(&source), None, 272000, 16384, 10).unwrap();
    assert_eq!(same.capacity_tokens, 400000);
    let other =
        resolve_model("gpt-6-sol", route_b, Some(&source), None, 272000, 16384, 10).unwrap();
    assert_eq!(
        other.capacity_tokens, 272000,
        "metadata from another destination must not establish route entitlement"
    );
    assert!(other.raw.context_window.is_none());
    assert_eq!(other.provenance["capacity"].status, "fallback");
    for mut destination in [
        Route::new("http://127.0.0.1:11001/other/responses", true),
        Route::new("http://127.0.0.1:11001/v1/responses", true),
    ] {
        destination.processing_tier = "priority".into();
        let other = resolve_model(
            "gpt-6-sol",
            destination,
            Some(&source),
            None,
            272000,
            16384,
            10,
        )
        .unwrap();
        assert_eq!(
            other.capacity_tokens, 272000,
            "route path and service tier qualify facts"
        );
        assert!(other.raw.context_window.is_none());
    }
}

#[test]
fn missing_invalid_stale_duplicate_and_maximum_only_metadata_stay_distinct() {
    let route = Route::new("http://127.0.0.1:11001/v1/responses", true);
    for (input, status, raw) in [
        (json!(null), "missing", None),
        (json!(0), "invalid", Some(0)),
        (json!(-1), "invalid", None),
        (json!("400000"), "invalid", None),
        (json!(2000001), "invalid", Some(2000001)),
    ] {
        let mut metadata = facts();
        metadata["context_window"] = input;
        let source = source(&route, metadata);
        let resolved = resolve_model(
            "gpt-6-sol",
            route.clone(),
            Some(&source),
            None,
            272000,
            16384,
            10,
        )
        .unwrap();
        assert_eq!(resolved.capacity_tokens, 272000);
        assert_eq!(resolved.raw_field_status["context_window"], status);
        assert_eq!(resolved.raw.context_window, raw);
        assert_eq!(resolved.raw.max_context_window, Some(800000));
    }
    let mut metadata = facts();
    metadata.as_object_mut().unwrap().remove("context_window");
    let source_max_only = source(&route, metadata);
    let maximum = resolve_model(
        "gpt-6-sol",
        route.clone(),
        Some(&source_max_only),
        None,
        272000,
        16384,
        10,
    )
    .unwrap();
    assert_eq!(
        maximum.capacity_tokens, 272000,
        "maximum config override is not an advertised default"
    );
    let mut stale = source(&route, facts());
    stale.status = "stale".into();
    let resolved = resolve_model(
        "gpt-6-sol",
        route.clone(),
        Some(&stale),
        None,
        272000,
        16384,
        10,
    )
    .unwrap();
    assert_eq!(resolved.capacity_tokens, 272000);
    assert_eq!(resolved.raw.context_window, Some(400000));
    assert!(resolved.provenance["capacity"].fallback_reason.is_some());
    assert!(SourceCatalog::from_codex(&json!({"models":[facts(),facts()]}), 123, true).is_err());
    assert!(SourceCatalog::from_codex(&json!({"models":vec![facts();2049]}), 123, true).is_err());
}

#[test]
fn contradictory_auto_compaction_is_preserved_but_not_validated_as_capacity() {
    let route = Route::new("http://127.0.0.1:11001/v1/responses", true);
    let mut metadata = facts();
    metadata["auto_compact_token_limit"] = json!(500000);
    let source = source(&route, metadata);
    let resolved =
        resolve_model("gpt-6-sol", route, Some(&source), None, 272000, 16384, 10).unwrap();
    assert_eq!(resolved.raw.auto_compact_token_limit, Some(500000));
    assert_eq!(
        resolved.raw_field_status["auto_compact_token_limit"],
        "invalid_relationship"
    );
    assert_eq!(
        resolved.capacity_tokens, 400000,
        "auto trigger is not the model capacity"
    );
}

#[test]
fn capacity_ordinary_target_and_legacy_modeled_rates_are_separate() {
    let route = Route::new("http://127.0.0.1:11001/v1/responses", true);
    let source = source(&route, facts());
    let resolved = resolve_model(
        "gpt-6-sol",
        route.clone(),
        Some(&source),
        None,
        272000,
        16384,
        10,
    )
    .unwrap();
    assert_eq!(resolved.capacity_tokens, 400000);
    assert_eq!(resolved.cheap_input_boundary, Some(272000));
    assert_eq!(resolved.ordinary.total_budget, 272000);
    assert_eq!(
        u64::from(resolved.ordinary.input_target),
        (272000u64 - 16384) * 100 / 110
    );
    assert!(resolved.ordinary.input_target < resolved.capacity_tokens);
    let short = resolved.rates_for_input(272000.0).unwrap();
    let long = resolved.rates_for_input(272001.0).unwrap();
    assert_eq!(long.input, short.input * 2.0);
    assert!((long.output - short.output * 1.5).abs() < 1e-18);
    assert_eq!(resolved.price_basis, "legacy_api_equivalent_model");
    assert!(
        resolved.cache.is_none(),
        "subscription metadata supplies no cache transport contract"
    );
    let gateway = resolve_model(
        "gpt-6-sol",
        Route::new("https://gateway.invalid/v1/responses", false),
        Some(&source),
        None,
        272000,
        16384,
        10,
    )
    .unwrap();
    assert_eq!(gateway.capacity_tokens, 272000);
    assert!(gateway.cache.is_none());
    assert!(!gateway.explicit_cache_transport);
    let unknown = resolve_model(
        "synthetic-unpriced",
        route.clone(),
        Some(&source),
        None,
        272000,
        16384,
        10,
    )
    .unwrap();
    assert!(unknown.rates.is_none());
    assert_eq!(unknown.price_basis, "unknown");
    assert!(
        resolve_model(
            "gpt-6-sol",
            route.clone(),
            Some(&source),
            Some(400001),
            272000,
            16384,
            10
        )
        .is_err()
    );
    let lower = resolve_model(
        "gpt-6-sol",
        route,
        Some(&source),
        Some(272000),
        272000,
        16384,
        10,
    )
    .unwrap();
    assert_eq!(lower.provenance["capacity"].operator_override, Some(272000));
    assert_eq!(lower.provenance["capacity"].status, "operator_override");
    assert_eq!(
        ordinary_policy(8000, Some(272000), 16384, 10).input_target,
        0
    );
}
